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
  - A creature component whose head/body don't sit exactly in the default
    zones may ship an ANCHOR SIDECAR ({same name}.json) declaring where
    they actually are: {"head_center": [x, y], "head_width": w,
    "body_center": [x, y], "body_width": w}. The compositor then shifts
    and scales the face/front layers to the head anchor and the
    back/detail layers to the body anchor, so every face variant fits
    every body variant. No sidecar means the default zones apply.

Directory listings and file bytes are cached for the process lifetime
(same policy as utils/pet_assets.py — new component packs need a restart).
"""

from __future__ import annotations

import json
import logging
from pathlib import Path

from PIL import Image

from domain.pet_constants import get_accessory
from utils.pet_drawing import (
    _DEFAULT_PALETTE,
    CARD_HEIGHT,
    CARD_WIDTH,
    FLOOR_Y,
    SLOT_ORDER,
    SPECIES_PALETTES,
    _geometry,
    _slot_rng,
    assemble_card,
    render_accessory,
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
            pool = sorted(directory.glob("*.png")) if directory.is_dir() else []
        except OSError:
            pool = []
        if not pool and slot == "backdrop" and stage != "adult":
            # Backdrops are scenes, not creatures — stage-agnostic. Reuse the
            # adult set instead of shipping byte-identical copies per stage.
            pool = _pool("adult", slot)
        _pool_cache[key] = pool
    return _pool_cache[key]


def manifest_token() -> int:
    """Stable token for the currently discovered component set, for use in
    byte-cache keys so composites re-render when the manifest differs."""
    names = []
    for stage in ("baby", "adult"):
        for slot in (*SLOT_ORDER, "accessory"):
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


# The frame disk components are AUTHORED against — the zone spec published
# in prompts.md. Adult matches the procedural geometry exactly; baby differs
# (AI baby heads sit high on chibi bodies, procedural chibi heads sit low).
AUTHORING_FRAMES = {
    "adult": {
        "head_center": [256, 64], "head_width": 64,
        "body_center": [256, 184], "body_width": 160,
    },
    "baby": {
        "head_center": [256, 95], "head_width": 80,
        "body_center": [256, 190], "body_width": 140,
    },
}


def _geometry_frame(stage: str) -> dict:
    """The frame procedural layers are DRAWN in, from the pixel geometry."""
    g = _geometry(stage)
    return {
        "head_center": [g.hx + g.hw // 2, g.hy + g.hh // 2],
        "head_width": g.hw,
        "body_center": [g.x0 + g.body_w // 2, g.body_top + (FLOOR_Y - g.body_top) // 2],
        "body_width": g.body_w,
    }


def _authoring_frame(stage: str) -> dict:
    return AUTHORING_FRAMES.get(stage, _geometry_frame(stage))


def _load_anchors(component_path: Path, stage: str) -> dict:
    """Target anchors for a disk creature: sidecar values over the authoring
    frame (a component with no sidecar is assumed to match the contract)."""
    anchors = dict(_authoring_frame(stage))
    sidecar = component_path.with_suffix(".json")
    key = f"anchor:{sidecar}"
    cached = _image_cache.get(key)
    if cached is None and sidecar.is_file():
        try:
            cached = json.loads(sidecar.read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            logger.warning("Bad pet anchor sidecar %s: %s", sidecar, exc)
            cached = {}
        _image_cache[key] = cached
    if cached:
        anchors.update(cached)
    return anchors


def _fit_layer(
    layer: Image.Image,
    *,
    scale: float,
    pivot: tuple[float, float],
    translate: tuple[float, float],
) -> Image.Image:
    """Scale a layer about `pivot`, then translate — used to move face/detail
    layers onto a creature component's actual anchors."""
    if abs(scale - 1.0) < 0.01 and translate == (0, 0):
        return layer
    px, py = pivot
    tx, ty = translate
    # PIL affine maps OUTPUT coords to INPUT coords.
    inv = 1.0 / scale
    coeffs = (
        inv, 0.0, px - (px + tx) * inv,
        0.0, inv, py - (py + ty) * inv,
    )
    return layer.transform(
        (CARD_WIDTH, CARD_HEIGHT), Image.AFFINE, coeffs, resample=Image.BICUBIC
    )


def _fit_to_target(layer: Image.Image, source: dict, target: dict, kind: str) -> Image.Image:
    """Map a layer from the frame it was authored/drawn in onto the
    creature's actual frame. kind is "head" (face/front) or "body"
    (back/detail)."""
    src_c, src_w = source[f"{kind}_center"], source[f"{kind}_width"]
    tgt_c, tgt_w = target[f"{kind}_center"], target[f"{kind}_width"]
    scale = tgt_w / src_w if src_w else 1.0
    return _fit_layer(
        layer,
        scale=scale,
        pivot=tuple(src_c),
        translate=(tgt_c[0] - src_c[0], tgt_c[1] - src_c[1]),
    )


def _accessory_layer(
    accessory_id: str, stage: str, seed: int, authoring: dict, geometry: dict, target: dict
) -> Image.Image | None:
    """Resolve a trinket layer: disk component (authored frame) or the
    procedural draw (geometry frame), anchored per the accessory's kind."""
    layer = None
    source = authoring
    pool = _pool(stage, "accessory")
    candidates = [p for p in pool if p.name.startswith(f"{accessory_id}")]
    if candidates:
        rng = _slot_rng(seed, "pick:accessory")
        layer = _load_component(rng.choice(candidates))
    if layer is None:
        layer = render_accessory(accessory_id, stage)
        source = geometry
    if layer is None:
        return None
    kind = "head" if get_accessory(accessory_id).anchor == "head" else "body"
    return _fit_to_target(layer, source, target, kind)


def compose_pet_card(
    species_id: str, stage: str, mood: str, seed: int, accessory: str | None = None
):
    """Compose a pet card from disk components with procedural fallback
    per slot. Returns io.BytesIO of the finished PNG.

    Frames: disk components are authored in the AUTHORING frame; procedural
    layers draw in the geometry frame. Anchored layers (face/front to the
    head, back/detail to the body) are mapped from their source frame onto
    the creature's target frame, so any face fits any body.
    """
    authoring = _authoring_frame(stage)
    geometry = _geometry_frame(stage)
    # Target frame comes from the creature (picked deterministically) and
    # must be known up front: the `back` slot composites before `creature`.
    creature_pick = _pick_component("creature", stage, species_id, mood, seed)
    if creature_pick is not None and _load_component(creature_pick[0]) is not None:
        target = _load_anchors(creature_pick[0], stage)
    else:
        target = geometry  # procedural creature
    layers = []
    for slot in SLOT_ORDER:
        layer: Image.Image | None = None
        source = authoring
        picked = _pick_component(slot, stage, species_id, mood, seed)
        if picked is not None:
            path, needs_tint = picked
            component = _load_component(path)
            if component is not None:
                layer = _tint_layer(component, species_id) if needs_tint else component
        if layer is None:
            layer = render_layer(slot, species_id, stage, mood, seed)
            source = geometry
        if layer is not None and slot in ("face", "front"):
            layer = _fit_to_target(layer, source, target, "head")
        elif layer is not None and slot in ("back", "detail"):
            layer = _fit_to_target(layer, source, target, "body")
        layers.append(layer)
    if accessory:
        # Trinkets composite above the face, below front features (orbs).
        layers.insert(
            SLOT_ORDER.index("front"),
            _accessory_layer(accessory, stage, seed, authoring, geometry, target),
        )
    return assemble_card(layers)
