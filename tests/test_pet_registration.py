"""Registration checks: faces and accessories must land on every body.

Unlike test_pet_compositor (which isolates a temp components dir), these
tests run against the REAL committed component pack, so they fail if art is
dropped in whose anchors drift from its sidecar, or if a sidecar goes stale.
"""

from __future__ import annotations

import pytest
from PIL import Image, ImageChops

from domain.pet_constants import ACCESSORIES
from utils import pet_compositor
from utils.pet_drawing import FLOOR_Y, render_layer


def _creature_components():
    for stage in ("adult", "baby"):
        for path in pet_compositor._pool(stage, "creature"):
            yield stage, path


@pytest.fixture(autouse=True)
def _fresh_caches():
    pet_compositor.clear_caches()
    yield
    pet_compositor.clear_caches()


def _bbox_center(img: Image.Image) -> tuple[int, int] | None:
    bbox = img.getchannel("A").getbbox()
    if bbox is None:
        return None
    return (bbox[0] + bbox[2]) // 2, (bbox[1] + bbox[3]) // 2


def _overlap_ratio(layer: Image.Image, creature: Image.Image) -> float:
    layer_mask = layer.getchannel("A").point(lambda value: 255 if value > 20 else 0)
    creature_mask = creature.getchannel("A").point(
        lambda value: 255 if value > 20 else 0
    )
    layer_area = layer_mask.histogram()[255]
    overlap = ImageChops.multiply(layer_mask, creature_mask)
    return overlap.histogram()[255] / layer_area


class TestRegistration:
    def test_every_creature_has_its_head_at_its_anchor(self):
        """The sidecar (or authoring frame) must point at actual head pixels."""
        checked = 0
        for stage, path in _creature_components():
            anchors = pet_compositor._load_anchors(path, stage)
            component = pet_compositor._load_component(path)
            assert component is not None, path
            cx, cy = anchors["head_center"]
            alpha = component.getchannel("A")
            # The declared head center must sit on opaque creature pixels.
            assert alpha.getpixel((cx, cy)) > 50, (
                f"{path.name} ({stage}): head anchor ({cx},{cy}) is not on "
                "the creature — sidecar is stale for this art"
            )
            checked += 1
        assert checked >= 6  # both stages, all shipped variants

    def test_face_layer_lands_on_the_declared_head(self):
        """End-to-end transform math: the procedural face, mapped onto each
        body's anchors, must center on that body's head."""
        for stage, path in _creature_components():
            anchors = pet_compositor._load_anchors(path, stage)
            geometry = pet_compositor._geometry_frame(stage)
            face = render_layer("face", "common_cama", stage, "happy", seed=1)
            fitted = pet_compositor._fit_to_target(face, geometry, anchors, "head")
            center = _bbox_center(fitted)
            assert center is not None
            dx = abs(center[0] - anchors["head_center"][0])
            dy = abs(center[1] - anchors["head_center"][1])
            assert dx <= 10 and dy <= 14, (
                f"{path.name} ({stage}): face landed at {center}, head at "
                f"{anchors['head_center']}"
            )

    def test_every_authored_face_is_fully_registered_to_every_creatures_head(
        self,
    ):
        """Every body keeps every applicable mood face contained and centered."""
        for stage, path in _creature_components():
            anchors = pet_compositor._load_anchors(path, stage)
            source = pet_compositor._authoring_frame(stage)
            creature = pet_compositor._load_component(path)
            assert creature is not None
            for face_path in pet_compositor._pool(stage, "face"):
                face = pet_compositor._load_component(face_path)
                assert face is not None
                mood = face_path.stem.split("_")[-2]
                mood_mount = f"face_{mood}"
                anchor_kind = (
                    mood_mount
                    if f"{mood_mount}_center" in anchors
                    and f"{mood_mount}_width" in anchors
                    else "face"
                )
                fitted = pet_compositor._fit_to_target(
                    face, source, anchors, anchor_kind
                )

                ratio = _overlap_ratio(fitted, creature)
                assert ratio == 1.0, (
                    f"{face_path.name} on {path.name} ({stage}): only "
                    f"{ratio:.1%} overlaps visible wool"
                )
                face_bbox = (
                    fitted.getchannel("A")
                    .point(lambda value: 255 if value > 20 else 0)
                    .getbbox()
                )
                assert face_bbox is not None
                head_x, head_y = anchors["head_center"]
                head_radius = round(anchors["head_width"] * 0.6)
                head_box = (
                    head_x - head_radius,
                    head_y - head_radius,
                    head_x + head_radius,
                    head_y + head_radius,
                )
                assert (
                    face_bbox[0] >= head_box[0]
                    and face_bbox[1] >= head_box[1]
                    and face_bbox[2] <= head_box[2]
                    and face_bbox[3] <= head_box[3]
                ), (
                    f"{face_path.name} on {path.name} ({stage}): face "
                    f"bounds {face_bbox} escape head region {head_box}"
                )
                face_center = (
                    (face_bbox[0] + face_bbox[2]) / 2,
                    (face_bbox[1] + face_bbox[3]) / 2,
                )
                target_center = anchors[f"{anchor_kind}_center"]
                dx = abs(face_center[0] - target_center[0])
                dy = abs(face_center[1] - target_center[1])
                assert dx <= 1 and dy <= 1, (
                    f"{face_path.name} on {path.name} ({stage}): face "
                    f"center {face_center} is offset from the "
                    f"{anchor_kind} mount {target_center}"
                )

    def test_every_accessory_is_visibly_attached_to_every_body(self):
        """Headwear must touch the head; neckwear and pins sit in the wool."""
        expected_mounts = {
            "red_bow": "headwear",
            "straw_hat": "headwear",
            "flower_crown": "headwear",
            "top_hat": "headwear",
            "bell_collar": "neck",
            "wool_scarf": "neck",
            "aegis_charm": "chest",
            "divine_rapier_pin": "chest",
        }
        for stage, path in _creature_components():
            anchors = pet_compositor._load_anchors(path, stage)
            authoring = pet_compositor._authoring_frame(stage)
            geometry = pet_compositor._geometry_frame(stage)
            component = pet_compositor._load_component(path)
            assert component is not None
            assert set(expected_mounts) == set(ACCESSORIES)
            for accessory_id, mount in expected_mounts.items():
                layer = pet_compositor._accessory_layer(
                    accessory_id, stage, 1, authoring, geometry, anchors
                )
                assert layer is not None, accessory_id
                minimum_overlap = 0.05 if mount == "headwear" else 0.80
                ratio = _overlap_ratio(layer, component)
                assert ratio >= minimum_overlap, (
                    f"{accessory_id} floats off {path.name} ({stage}): "
                    f"{ratio:.1%} overlap, expected at least "
                    f"{minimum_overlap:.0%}"
                )

    @pytest.mark.parametrize(
        "species_id", ["riverglow_cama", "prismwool_cama"]
    )
    def test_ground_effects_stay_on_the_floor_for_every_body(self, species_id):
        """Floor-relative effects may resize horizontally, never vertically."""
        for stage, path in _creature_components():
            geometry = pet_compositor._geometry_frame(stage)
            anchors = pet_compositor._load_anchors(path, stage)
            common = render_layer(
                "ground", "common_cama", stage, "neutral", seed=17
            )
            species = render_layer(
                "ground", species_id, stage, "neutral", seed=17
            )
            assert common is not None
            assert species is not None

            fitted_common = pet_compositor._fit_ground_to_target(
                common, geometry, anchors
            )
            fitted_species = pet_compositor._fit_ground_to_target(
                species, geometry, anchors
            )
            effect_bbox = ImageChops.difference(
                fitted_species, fitted_common
            ).convert("RGB").getbbox()

            assert effect_bbox is not None
            assert FLOOR_Y - 8 <= effect_bbox[1] <= FLOOR_Y
            assert FLOOR_Y <= effect_bbox[3] <= FLOOR_Y + 12
