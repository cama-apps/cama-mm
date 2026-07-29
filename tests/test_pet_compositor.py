"""Tests for the hybrid pet layer compositor (disk components + procedural)."""

from __future__ import annotations

import json

import pytest
from PIL import Image

from utils import pet_compositor
from utils.pet_drawing import CARD_HEIGHT, CARD_WIDTH, render_pet_card

# Adult geometry landmarks (cell=16): head box x224-288 y40-88, body x176-336 y120-200.
FACE_BOX = (224, 40, 288, 88)
BODY_BOX = (176, 120, 336, 200)
FACE_PX = (256, 60)
BODY_PX = (256, 160)

MAGENTA = (255, 0, 255, 255)
BLUE = (0, 80, 255, 255)


@pytest.fixture(autouse=True)
def _isolated_components(tmp_path, monkeypatch):
    """Point the compositor at a per-test components dir and reset caches."""
    monkeypatch.setattr(pet_compositor, "COMPONENTS_DIR", tmp_path / "components")
    pet_compositor.clear_caches()
    yield
    pet_compositor.clear_caches()


def write_component(root, stage, slot, name, box, color):
    directory = root / "components" / stage / slot
    directory.mkdir(parents=True, exist_ok=True)
    img = Image.new("RGBA", (CARD_WIDTH, CARD_HEIGHT), (0, 0, 0, 0))
    img.paste(Image.new("RGBA", (box[2] - box[0], box[3] - box[1]), color), box[:2])
    img.save(directory / f"{name}.png")


def compose_image(species="common_cama", stage="adult", mood="happy", seed=7):
    return Image.open(
        pet_compositor.compose_pet_card(species, stage, mood, seed)
    ).convert("RGBA")


class TestComposition:
    def test_no_components_matches_pure_procedural(self):
        composed = pet_compositor.compose_pet_card(
            "common_cama", "adult", "happy", seed=7
        ).getvalue()
        procedural = render_pet_card("common_cama", "adult", "happy", seed=7).getvalue()
        assert composed == procedural

    def test_disk_face_component_wins_its_slot_only(self, tmp_path):
        write_component(tmp_path, "adult", "face", "any_happy_01", FACE_BOX, MAGENTA)
        pet_compositor.clear_caches()
        img = compose_image()
        assert img.getpixel(FACE_PX)[:3] == MAGENTA[:3]  # face replaced
        # Creature slot still procedural: body pixel is the species wool,
        # identical to the fully procedural render (slot independence).
        procedural = Image.open(
            render_pet_card("common_cama", "adult", "happy", seed=7)
        ).convert("RGBA")
        assert img.getpixel(BODY_PX) == procedural.getpixel(BODY_PX)

    def test_species_scoped_beats_shared(self, tmp_path):
        write_component(tmp_path, "adult", "face", "any_happy_01", FACE_BOX, MAGENTA)
        write_component(tmp_path, "adult", "face", "rama_happy_01", FACE_BOX, BLUE)
        pet_compositor.clear_caches()
        assert compose_image(species="rama").getpixel(FACE_PX)[:3] == BLUE[:3]
        assert compose_image(species="common_cama").getpixel(FACE_PX)[:3] == MAGENTA[:3]

    def test_face_components_are_mood_keyed(self, tmp_path):
        write_component(tmp_path, "adult", "face", "any_hungry_01", FACE_BOX, MAGENTA)
        pet_compositor.clear_caches()
        assert compose_image(mood="hungry").getpixel(FACE_PX)[:3] == MAGENTA[:3]
        assert compose_image(mood="happy").getpixel(FACE_PX)[:3] != MAGENTA[:3]

    def test_shared_creature_component_is_species_tinted(self, tmp_path):
        gray = (128, 128, 128, 255)
        write_component(tmp_path, "adult", "creature", "any_01", BODY_BOX, gray)
        pet_compositor.clear_caches()
        frost = compose_image(species="crystal_cama").getpixel(BODY_PX)
        gold = compose_image(species="jopacama").getpixel(BODY_PX)
        assert frost[2] > frost[0]  # Frostwool tint is blue-leaning
        assert gold[0] > gold[2]  # Gilded tint is gold-leaning
        assert frost != gold

    def test_species_scoped_component_is_not_tinted(self, tmp_path):
        write_component(tmp_path, "adult", "creature", "rama_01", BODY_BOX, MAGENTA)
        pet_compositor.clear_caches()
        # Probe a flank pixel left of Rama's royal blanket (the detail slot
        # legitimately composites over the creature's mid-body).
        flank_px = (196, 190)
        assert compose_image(species="rama").getpixel(flank_px)[:3] == MAGENTA[:3]

    def test_variant_choice_is_seeded_and_stable(self, tmp_path):
        write_component(tmp_path, "adult", "face", "any_happy_01", FACE_BOX, MAGENTA)
        write_component(tmp_path, "adult", "face", "any_happy_02", FACE_BOX, BLUE)
        pet_compositor.clear_caches()
        first = pet_compositor.compose_pet_card(
            "common_cama", "adult", "happy", seed=7
        ).getvalue()
        second = pet_compositor.compose_pet_card(
            "common_cama", "adult", "happy", seed=7
        ).getvalue()
        assert first == second  # same pet, same look, always
        picks = {
            pet_compositor._pick_component("face", "adult", "common_cama", "happy", s)[0].name
            for s in range(40)
        }
        assert picks == {"any_happy_01.png", "any_happy_02.png"}  # variety across pets

    def test_anchor_sidecar_moves_face_onto_the_creatures_head(self, tmp_path):
        """A creature component may declare where its head ACTUALLY is; the
        face layer must follow it."""
        write_component(tmp_path, "adult", "creature", "any_01", BODY_BOX, (90, 90, 90, 255))
        write_component(tmp_path, "adult", "face", "any_happy_01", FACE_BOX, MAGENTA)
        sidecar = tmp_path / "components" / "adult" / "creature" / "any_01.json"
        sidecar.write_text(json.dumps({
            "head_center": [300, 64], "head_width": 64,
            "body_center": [256, 184], "body_width": 160,
        }))
        pet_compositor.clear_caches()
        img = compose_image()
        # Face translated +44px right onto the declared head position.
        assert img.getpixel((300, 60))[:3] == MAGENTA[:3]
        assert img.getpixel(FACE_PX)[:3] != MAGENTA[:3]

    @pytest.mark.parametrize(
        ("stage", "expected"),
        [
            (
                "adult",
                {
                    "face_center": [256, 68],
                    "face_width": 64,
                    "headwear_center": [256, 32],
                    "headwear_width": 64,
                    "neck_center": [256, 100],
                    "neck_width": 64,
                    "chest_center": [256, 115],
                    "chest_width": 64,
                },
            ),
            (
                "baby",
                {
                    "face_center": [256, 100],
                    "face_width": 80,
                    "headwear_center": [256, 55],
                    "headwear_width": 80,
                    "neck_center": [256, 138],
                    "neck_width": 80,
                    "chest_center": [256, 149],
                    "chest_width": 80,
                },
            ),
        ],
    )
    def test_component_without_sidecar_keeps_authoring_mounts(
        self, tmp_path, stage, expected
    ):
        write_component(
            tmp_path, stage, "creature", "any_01", BODY_BOX, (90, 90, 90, 255)
        )
        component_path = (
            tmp_path / "components" / stage / "creature" / "any_01.png"
        )

        anchors = pet_compositor._load_anchors(component_path, stage)

        assert {key: anchors[key] for key in expected} == expected

    def test_legacy_sidecar_derives_missing_semantic_mounts(self, tmp_path):
        write_component(
            tmp_path, "adult", "creature", "any_01", BODY_BOX, (90, 90, 90, 255)
        )
        component_path = (
            tmp_path / "components" / "adult" / "creature" / "any_01.png"
        )
        component_path.with_suffix(".json").write_text(
            json.dumps(
                {
                    "head_center": [300, 70],
                    "head_width": 60,
                    "body_center": [270, 190],
                    "body_width": 150,
                }
            )
        )

        anchors = pet_compositor._load_anchors(component_path, "adult")

        assert anchors["face_center"] == [300, 70]
        assert anchors["face_width"] == 60
        assert anchors["headwear_center"] == [300, 40]
        assert anchors["neck_center"] == [300, 103]
        assert anchors["chest_center"] == [285, 128]

    def test_mood_specific_face_mount_overrides_generic_mount(self, tmp_path):
        write_component(
            tmp_path, "adult", "creature", "any_01", BODY_BOX, (90, 90, 90, 255)
        )
        write_component(
            tmp_path, "adult", "face", "any_hungry_01", FACE_BOX, MAGENTA
        )
        sidecar = tmp_path / "components" / "adult" / "creature" / "any_01.json"
        sidecar.write_text(
            json.dumps(
                {
                    "head_center": [256, 64],
                    "head_width": 64,
                    "face_center": [256, 68],
                    "face_width": 64,
                    "face_hungry_center": [300, 68],
                    "face_hungry_width": 64,
                    "body_center": [256, 184],
                    "body_width": 160,
                }
            )
        )
        pet_compositor.clear_caches()

        img = compose_image(mood="hungry")

        assert img.getpixel((300, 68))[:3] == MAGENTA[:3]
        assert img.getpixel((256, 68))[:3] != MAGENTA[:3]

    def test_baby_backdrops_fall_back_to_adult_pool(self, tmp_path):
        """Backdrops are stage-agnostic scenes: no baby dir means reuse the
        adult set instead of shipping duplicate blobs."""
        backdrop = (255, 140, 0, 255)
        write_component(tmp_path, "adult", "backdrop", "any_01",
                        (0, 0, CARD_WIDTH, CARD_HEIGHT), backdrop)
        pet_compositor.clear_caches()
        img = compose_image(stage="baby")
        # An upper-left pixel (inside the vignette, clear of the creature)
        # shows the adult backdrop.
        assert img.getpixel((40, 40))[:3] == backdrop[:3]

    def test_manifest_token_tracks_component_set(self, tmp_path):
        empty = pet_compositor.manifest_token()
        write_component(tmp_path, "adult", "face", "any_happy_01", FACE_BOX, MAGENTA)
        pet_compositor.clear_caches()
        assert pet_compositor.manifest_token() != empty

    def test_unreadable_component_falls_back_to_procedural(self, tmp_path):
        directory = tmp_path / "components" / "adult" / "face"
        directory.mkdir(parents=True)
        (directory / "any_happy_01.png").write_bytes(b"not a png")
        pet_compositor.clear_caches()
        composed = pet_compositor.compose_pet_card(
            "common_cama", "adult", "happy", seed=7
        ).getvalue()
        assert composed == render_pet_card(
            "common_cama", "adult", "happy", seed=7
        ).getvalue()
