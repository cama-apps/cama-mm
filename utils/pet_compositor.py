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
  - A creature component whose parts don't sit exactly in the default
    zones may ship an ANCHOR SIDECAR ({same name}.json). The original
    head_center/head_width and body_center/body_width values remain
    supported; face, headwear, neck, and chest center/width pairs can
    refine individual mounts. Missing semantic mounts derive from the
    head/body frame, so older component packs remain compatible.

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
    render_evolution_motif,
    render_layer,
)

logger = logging.getLogger(__name__)

COMPONENTS_DIR = Path(__file__).resolve().parent.parent / "assets" / "pets" / "components"

# Slots whose shared ("any"-scoped) parts get species-palette tinting.
TINTED_SLOTS = {"ground", "back", "creature", "detail"}
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
        "face_center": [256, 68], "face_width": 64,
        "face_content_width": 42, "face_content_height": 25,
        "headwear_center": [256, 32], "headwear_width": 64,
        "neck_center": [256, 100], "neck_width": 64,
        "chest_center": [256, 115], "chest_width": 64,
        "body_center": [256, 184], "body_width": 160,
    },
    "baby": {
        "head_center": [256, 95], "head_width": 80,
        "face_center": [256, 100], "face_width": 80,
        "face_content_width": 64, "face_content_height": 33,
        "headwear_center": [256, 55], "headwear_width": 80,
        "neck_center": [256, 138], "neck_width": 80,
        "chest_center": [256, 149], "chest_width": 80,
        "body_center": [256, 190], "body_width": 140,
    },
}


def _geometry_frame(stage: str) -> dict:
    """The frame procedural layers are DRAWN in, from the pixel geometry."""
    g = _geometry(stage)
    return {
        "head_center": [g.hx + g.hw // 2, g.hy + g.hh // 2],
        "head_width": g.hw,
        "face_center": [g.hx + g.hw // 2, g.hy + g.hh // 2],
        "face_width": g.hw,
        "headwear_center": [g.hx + g.hw // 2, g.hy],
        "headwear_width": g.hw,
        "neck_center": [g.hx + g.hw // 2, g.hy + g.hh + g.cell // 4],
        "neck_width": g.hw,
        "chest_center": [
            g.hx + g.hw // 2,
            g.hy + g.hh + int(1.2 * g.cell),
        ],
        "chest_width": g.hw,
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
    else:
        return anchors
    # Old or third-party sidecars may only carry the original head/body
    # frame. Derive safe semantic mounts from that frame while allowing
    # authored sidecars to override every value independently.
    head_x, head_y = anchors["head_center"]
    head_width = anchors["head_width"]
    body_x, body_y = anchors["body_center"]
    semantic_defaults = {
        "face_center": [head_x, head_y],
        "face_width": head_width,
        "headwear_center": [head_x, round(head_y - head_width / 2)],
        "headwear_width": head_width,
        "neck_center": [head_x, round(head_y + head_width * 0.55)],
        "neck_width": head_width,
        "chest_center": [
            round((head_x + body_x) / 2),
            round(head_y + (body_y - head_y) * 0.48),
        ],
        "chest_width": head_width,
    }
    for key, value in semantic_defaults.items():
        if key not in cached:
            anchors[key] = value
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


def _normalized_face_source(layer: Image.Image, source: dict) -> dict:
    """Center authored face content and normalize differently sized moods."""
    reference_width = source.get("face_content_width")
    reference_height = source.get("face_content_height")
    if not reference_width or not reference_height:
        return source
    bbox = (
        layer.getchannel("A")
        .point(lambda value: 255 if value > 20 else 0)
        .getbbox()
    )
    if bbox is None:
        return source
    content_width = bbox[2] - bbox[0]
    content_height = bbox[3] - bbox[1]
    fitted_width = (
        content_width + 2 if content_width > reference_width else reference_width
    )
    fitted_height = (
        content_height + 2
        if content_height > reference_height
        else reference_height
    )
    footprint_scale = max(
        fitted_width / reference_width,
        fitted_height / reference_height,
    )
    normalized = dict(source)
    normalized["face_center"] = [
        (bbox[0] + bbox[2]) / 2,
        (bbox[1] + bbox[3]) / 2,
    ]
    normalized["face_width"] = source["face_width"] * footprint_scale
    return normalized


def _fit_to_target(layer: Image.Image, source: dict, target: dict, kind: str) -> Image.Image:
    """Map a layer from the frame it was authored/drawn in onto the
    creature's actual frame. Mood-specific face mounts share the authored
    face source frame."""
    source_kind = "face" if kind.startswith("face_") else kind
    if source_kind == "face":
        source = _normalized_face_source(layer, source)
    src_c = source[f"{source_kind}_center"]
    src_w = source[f"{source_kind}_width"]
    tgt_c, tgt_w = target[f"{kind}_center"], target[f"{kind}_width"]
    scale = tgt_w / src_w if src_w else 1.0
    return _fit_layer(
        layer,
        scale=scale,
        pivot=tuple(src_c),
        translate=(tgt_c[0] - src_c[0], tgt_c[1] - src_c[1]),
    )


def _fit_ground_to_target(
    layer: Image.Image, source: dict, target: dict
) -> Image.Image:
    """Fit floor art to the creature's width without moving it off FLOOR_Y."""
    source_x = source["body_center"][0]
    source_width = source["body_width"]
    target_x = target["body_center"][0]
    target_width = target["body_width"]
    scale = target_width / source_width if source_width else 1.0
    if abs(scale - 1.0) < 0.01 and source_x == target_x:
        return layer
    inverse = 1.0 / scale
    return layer.transform(
        (CARD_WIDTH, CARD_HEIGHT),
        Image.AFFINE,
        (
            inverse,
            0.0,
            source_x - target_x * inverse,
            0.0,
            1.0,
            0.0,
        ),
        resample=Image.BICUBIC,
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
    kind = get_accessory(accessory_id).anchor
    return _fit_to_target(layer, source, target, kind)


def compose_pet_card(
    species_id: str,
    stage: str,
    mood: str,
    seed: int,
    accessory: str | None = None,
    *,
    calling=None,
    primary=None,
    secondary=None,
):
    """Compose a pet card from disk components with procedural fallback
    per slot. Returns io.BytesIO of the finished PNG.

    Frames: disk components are authored in the AUTHORING frame; procedural
    layers draw in the geometry frame. Face, headwear, neckwear, and chest
    accessories use independent semantic mounts; front features still use
    the head and back/detail layers use the body.
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
        if layer is not None and slot == "ground":
            layer = _fit_ground_to_target(layer, source, target)
        elif layer is not None and slot == "face":
            mood_mount = f"face_{mood}"
            kind = (
                mood_mount
                if f"{mood_mount}_center" in target
                and f"{mood_mount}_width" in target
                else "face"
            )
            layer = _fit_to_target(layer, source, target, kind)
        elif layer is not None and slot == "front":
            layer = _fit_to_target(layer, source, target, "head")
        elif layer is not None and slot in ("back", "detail"):
            layer = _fit_to_target(layer, source, target, "body")
            if (
                slot == "detail"
                and stage == "adult"
                and species_id in ("courier_cama", "rama")
            ):
                layer = _fit_layer(
                    layer,
                    scale=1.0,
                    pivot=(0, 0),
                    translate=(
                        target["chest_center"][0] - target["body_center"][0],
                        0,
                    ),
                )
        layers.append(layer)
    if accessory:
        # Trinkets composite above the face, below front features (orbs).
        layers.insert(
            SLOT_ORDER.index("front"),
            _accessory_layer(accessory, stage, seed, authoring, geometry, target),
        )
    layers.insert(
        SLOT_ORDER.index("front"),
        render_evolution_motif(calling, primary, secondary, stage, seed),
    )
    return assemble_card(layers)
