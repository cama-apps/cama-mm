"""Registration checks: faces and accessories must land on every body.

Unlike test_pet_compositor (which isolates a temp components dir), these
tests run against the REAL committed component pack, so they fail if art is
dropped in whose anchors drift from its sidecar, or if a sidecar goes stale.
"""

from __future__ import annotations

import pytest
from PIL import Image

from domain.pet_constants import ACCESSORIES
from utils import pet_compositor
from utils.pet_drawing import render_accessory, render_layer


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

    def test_every_accessory_touches_every_body(self):
        """No trinket may float off the creature on any body variant."""
        for stage, path in _creature_components():
            anchors = pet_compositor._load_anchors(path, stage)
            geometry = pet_compositor._geometry_frame(stage)
            component = pet_compositor._load_component(path)
            body_bbox = component.getchannel("A").getbbox()
            grown = (
                body_bbox[0] - 12, body_bbox[1] - 24,
                body_bbox[2] + 12, body_bbox[3] + 12,
            )
            for accessory_id in ACCESSORIES:
                layer = render_accessory(accessory_id, stage)
                assert layer is not None, accessory_id
                fitted = pet_compositor._fit_to_target(
                    layer, geometry, anchors, "head"
                )
                abox = fitted.getchannel("A").getbbox()
                assert abox is not None, (accessory_id, path.name)
                overlaps = not (
                    abox[2] < grown[0] or abox[0] > grown[2]
                    or abox[3] < grown[1] or abox[1] > grown[3]
                )
                assert overlaps, (
                    f"{accessory_id} floats off {path.name} ({stage}): "
                    f"accessory {abox} vs body {grown}"
                )
