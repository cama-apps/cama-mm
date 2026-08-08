"""Tests for cama pet art: procedural rendering and the asset loader."""

import io

import discord
import pytest
from PIL import Image

from domain.pet_constants import SPECIES
from domain.pet_evolution import PetCalling, PetInstinct
from utils import pet_assets
from utils.pet_compositor import compose_pet_card
from utils.pet_drawing import (
    render_egg_card,
    render_layer,
    render_pet_card,
    render_tombstone_card,
)

STAGES = ("baby", "adult")
MOODS = ("happy", "neutral", "hungry")


def _assert_valid_card(buf):
    with Image.open(buf) as img:
        assert img.format == "PNG"
        assert img.size == (512, 288)


class TestRenderPetCard:
    def test_same_seed_is_deterministic(self):
        first = render_pet_card("common_cama", "baby", "happy", seed=42).getvalue()
        second = render_pet_card("common_cama", "baby", "happy", seed=42).getvalue()
        assert first == second

    def test_different_seed_changes_output(self):
        base = render_pet_card("common_cama", "baby", "happy", seed=42).getvalue()
        other = render_pet_card("common_cama", "baby", "happy", seed=43).getvalue()
        assert base != other

    def test_all_species_stages_moods_render_valid_pngs(self):
        """Every species renders a valid card at both stages, and every mood
        renders at both stages for one species. The mood axis selects the
        face/expression variant independently of the species sprite (see
        test_mood_variants_differ), so the full 15x2x3 grid re-rendered the
        same code paths 90 times for no extra coverage."""
        for species_id in SPECIES:
            for stage in STAGES:
                _assert_valid_card(render_pet_card(species_id, stage, "neutral", seed=7))
        for stage in STAGES:
            for mood in MOODS:
                _assert_valid_card(render_pet_card("common_cama", stage, mood, seed=7))

    def test_mood_variants_differ(self):
        happy = render_pet_card("jopacama", "adult", "happy", seed=5).getvalue()
        hungry = render_pet_card("jopacama", "adult", "hungry", seed=5).getvalue()
        assert happy != hungry

    def test_stage_variants_differ(self):
        baby = render_pet_card("jopacama", "baby", "neutral", seed=5).getvalue()
        adult = render_pet_card("jopacama", "adult", "neutral", seed=5).getvalue()
        assert baby != adult

    def test_species_sprites_differ_from_each_other(self):
        renders = {
            species_id: render_pet_card(species_id, "adult", "neutral", seed=5).getvalue()
            for species_id in SPECIES
        }
        assert len(set(renders.values())) == len(SPECIES)

    def test_new_species_have_distinct_composed_visuals(self):
        renders = {
            species_id: compose_pet_card(
                species_id, "adult", "neutral", seed=17
            ).getvalue()
            for species_id in (
                "common_cama",
                "embergear_cama",
                "riverglow_cama",
                "prismwool_cama",
                "moondrift_cama",
                "sunspun_cama",
            )
        }

        assert len(set(renders.values())) == len(renders)

    def test_legendary_species_have_signature_layers(self):
        common_ground = render_layer(
            "ground", "common_cama", "adult", "neutral", seed=17
        )
        moon_ground = render_layer(
            "ground", "moondrift_cama", "adult", "neutral", seed=17
        )
        common_back = render_layer(
            "back", "common_cama", "adult", "neutral", seed=17
        )
        sun_back = render_layer(
            "back", "sunspun_cama", "adult", "neutral", seed=17
        )

        assert common_ground is not None
        assert moon_ground is not None
        assert common_back is not None
        assert sun_back is not None
        assert moon_ground.tobytes() != common_ground.tobytes()
        assert sun_back.tobytes() != common_back.tobytes()

    def test_riverglow_uses_grounded_effect_instead_of_floating_orbs(self):
        front = render_layer("front", "riverglow_cama", "adult", "neutral", seed=17)
        river = render_layer(
            "ground", "riverglow_cama", "adult", "neutral", seed=17
        )
        common = render_layer(
            "ground", "common_cama", "adult", "neutral", seed=17
        )

        assert front is not None
        assert front.getchannel("A").getbbox() is None
        assert river is not None
        assert common is not None
        assert river.tobytes() != common.tobytes()

    def test_riverglow_body_mark_stays_compact(self):
        detail = render_layer(
            "detail", "riverglow_cama", "adult", "neutral", seed=17
        )

        assert detail is not None
        bbox = detail.getchannel("A").getbbox()
        assert bbox is not None
        assert bbox[2] - bbox[0] <= 20

    def test_prismwool_casts_prismatic_light_beyond_its_fleece(self):
        prism = render_layer(
            "ground", "prismwool_cama", "adult", "neutral", seed=17
        )
        common = render_layer(
            "ground", "common_cama", "adult", "neutral", seed=17
        )

        assert prism is not None
        assert common is not None
        assert prism.tobytes() != common.tobytes()


class TestRenderEggAndTombstone:
    def test_egg_renders_valid_png(self):
        _assert_valid_card(render_egg_card(seed=11))

    def test_tombstone_renders_valid_png(self):
        _assert_valid_card(render_tombstone_card("Fluffy"))

    def test_tombstone_handles_long_name(self):
        _assert_valid_card(render_tombstone_card("A" * 60))


class TestPetAssetLoader:
    @pytest.fixture(autouse=True)
    def _empty_assets_dir(self, tmp_path, monkeypatch):
        """Point the loader at an empty dir so the procedural tier is exercised."""
        monkeypatch.setattr(pet_assets, "ASSETS_DIR", tmp_path / "pets")
        pet_assets._bytes_cache.clear()
        yield
        pet_assets._bytes_cache.clear()

    def test_get_pet_card_procedural_fallback_filename(self):
        file = pet_assets.get_pet_card("common_cama", "adult", "happy", seed=3)
        assert isinstance(file, discord.File)
        assert file.filename == "pet_common_cama_adult_happy.png"

    def test_get_pet_card_returns_fresh_file_objects(self):
        first = pet_assets.get_pet_card("rama", "baby", "neutral", seed=9)
        second = pet_assets.get_pet_card("rama", "baby", "neutral", seed=9)
        assert first is not None
        assert second is not None
        assert first is not second
        assert first.fp is not second.fp

    def test_evolved_card_bytes_are_rendered_once_and_then_cached(self, monkeypatch):
        from utils import pet_compositor

        calls = 0
        original = pet_compositor.compose_pet_card

        def counted(*args, **kwargs):
            nonlocal calls
            calls += 1
            return original(*args, **kwargs)

        monkeypatch.setattr(pet_compositor, "compose_pet_card", counted)
        kwargs = {
            "calling": PetCalling.PROSPECTOR,
            "primary": PetInstinct.DELVING,
            "secondary": PetInstinct.FORTUNE,
        }

        first = pet_assets.get_pet_card(
            "common_cama", "adult", "happy", seed=13, **kwargs
        )
        second = pet_assets.get_pet_card(
            "common_cama", "adult", "happy", seed=13, **kwargs
        )

        assert first is not second
        assert calls == 1

    def test_get_egg_card_procedural_fallback(self):
        file = pet_assets.get_egg_card(seed=4)
        assert isinstance(file, discord.File)
        assert file.filename == "pet_egg.png"

    def test_get_tombstone_card_procedural_fallback(self):
        file = pet_assets.get_tombstone_card("Fluffy", seed=4)
        assert isinstance(file, discord.File)
        assert file.filename == "pet_tombstone.png"

    def test_get_tombstone_card_engraves_name_on_disk_asset(self):
        pet_assets.ASSETS_DIR.mkdir(parents=True)
        Image.new("RGBA", (512, 288), (150, 150, 160, 255)).save(
            pet_assets.ASSETS_DIR / "tombstone.png"
        )

        alpha = pet_assets.get_tombstone_card("Alpha", seed=1).fp.read()
        beta = pet_assets.get_tombstone_card("Beta", seed=2).fp.read()

        assert alpha != beta

    def test_get_versus_card_composes_two_renders(self):
        file = pet_assets.get_versus_card(
            "common_cama", "adult", 3, None, "rama", "baby", 7, "red_bow"
        )
        assert isinstance(file, discord.File)
        assert file.filename == "pet_versus.png"
        with Image.open(file.fp) as img:
            # Two 512-wide cards side by side.
            assert img.width == 1024

    def test_get_altar_card_falls_back_to_tombstone(self):
        file = pet_assets.get_altar_card("Fluffy", seed=4)
        assert isinstance(file, discord.File)
        assert file.filename == "pet_tombstone.png"

    def test_get_altar_card_prefers_authored_asset(self):
        pet_assets.ASSETS_DIR.mkdir(parents=True)
        Image.new("RGBA", (512, 288), (40, 20, 60, 255)).save(
            pet_assets.ASSETS_DIR / "altar.png"
        )
        file = pet_assets.get_altar_card("Fluffy", seed=4)
        assert file.filename == "pet_altar.png"

    def test_get_versus_card_prefers_authored_overlay(self):
        baseline = pet_assets.get_versus_card(
            "common_cama", "adult", 3, None, "rama", "baby", 7, None
        ).fp.read()
        pet_assets.ASSETS_DIR.mkdir(parents=True)
        # A loud opaque overlay so the difference is unambiguous.
        Image.new("RGBA", (1024, 288), (255, 0, 0, 255)).save(
            pet_assets.ASSETS_DIR / "versus_overlay.png"
        )
        overlaid = pet_assets.get_versus_card(
            "common_cama", "adult", 3, None, "rama", "baby", 7, None
        ).fp.read()
        assert overlaid != baseline
        with Image.open(io.BytesIO(overlaid)) as img:
            assert img.convert("RGBA").getpixel((0, 0)) == (255, 0, 0, 255)
