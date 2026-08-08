"""Tests for dig minigame asset loading and pixel art generation."""

import asyncio
import io
import threading
from unittest.mock import MagicMock

import discord
import pytest
from PIL import Image, ImageChops

from utils import dig_assets, dig_drawing
from utils.dig_assets import (
    _MAX_FILE_SIZE,
    ASSETS_DIR,
    _find_asset,
    _load_cached_bytes,
    get_boss_art,
    get_event_art,
    get_layer_thumbnail,
)
from utils.dig_drawing import (
    LAYER_PALETTES,
    draw_boss_result_scene,
    draw_boss_scene,
    draw_event_scene,
    draw_layer_thumbnail,
    has_event_scene,
)

NEW_PRESTIGE2_BOSS_ASSET_IDS = (
    "cairn_general",
    "brass_croupier",
    "saltveil_commodore",
)
BOSS_ART_STATES = ("encounter", "victory", "defeat")
PINNACLE_BOSS_IDS = (
    "forgotten_king",
    "hollowforged",
    "first_digger",
    "bastion_below",
    "pale_surveyor",
    "lantern_engine",
)
MAX_STRONGLY_CHANGED_FRACTION = 0.05
NEW_PINNACLE_BOSS_ASSET_IDS = (
    "bastion_below",
    "pale_surveyor",
    "lantern_engine",
)


def _encounter_png_bytes(color=(80, 50, 30, 255)) -> bytes:
    buf = io.BytesIO()
    Image.new("RGBA", (512, 288), color).save(buf, format="PNG")
    return buf.getvalue()


def _clear_pinnacle_phase_state() -> None:
    dig_assets._pinnacle_phase_art_cache.clear()
    getattr(dig_assets, "_pinnacle_phase_art_inflight", {}).clear()
    getattr(dig_assets, "_pinnacle_phase_art_failures", set()).clear()


def _strongly_changed_fraction(
    actual: Image.Image,
    reference: Image.Image,
    *,
    threshold: int = 55,
) -> float:
    difference = ImageChops.difference(
        actual.convert("RGB"),
        reference.convert("RGB"),
    )
    red, green, blue = difference.split()
    max_channel_delta = ImageChops.lighter(
        ImageChops.lighter(red, green),
        blue,
    )
    histogram = max_channel_delta.histogram()
    changed_pixels = sum(histogram[threshold + 1:])
    return changed_pixels / (actual.width * actual.height)


# Both pinnacle phase renders are deterministic (proven by
# test_pinnacle_phase3_effects_are_deterministic), so each (boss_id, secret)
# pair is rendered once per process and shared across the assertion tests
# below instead of re-rendering per test.
_pinnacle_render_cache: dict[tuple[str, bool], tuple[bytes, bytes]] = {}


def _pinnacle_phases(boss_id: str, secret: bool) -> tuple[bytes, bytes]:
    """Return (phase2_png_bytes, phase3_gif_bytes) for the real boss asset."""
    key = (boss_id, secret)
    if key not in _pinnacle_render_cache:
        source = (ASSETS_DIR / "bosses" / f"{boss_id}_encounter.png").read_bytes()
        phase2 = dig_drawing.draw_pinnacle_phase2(
            source, boss_id, secret=secret,
        ).getvalue()
        phase3 = dig_drawing.animate_pinnacle_phase3(
            source, boss_id, secret=secret,
        ).getvalue()
        _pinnacle_render_cache[key] = (phase2, phase3)
    return _pinnacle_render_cache[key]


# =============================================================================
# dig_drawing tests
# =============================================================================


class TestDrawLayerThumbnail:
    """Tests for layer thumbnail generation."""

    def test_returns_bytesio_with_valid_png(self):
        buf = draw_layer_thumbnail("Dirt")
        assert isinstance(buf, io.BytesIO)
        img = Image.open(buf)
        assert img.format == "PNG"
        assert img.size == (128, 128)

    def test_all_layers_produce_thumbnails(self):
        for layer_name in LAYER_PALETTES:
            buf = draw_layer_thumbnail(layer_name)
            img = Image.open(buf)
            assert img.size == (128, 128), f"Failed for {layer_name}"

    def test_deterministic_output(self):
        buf1 = draw_layer_thumbnail("Stone")
        buf2 = draw_layer_thumbnail("Stone")
        assert buf1.getvalue() == buf2.getvalue()


class TestDrawBossScene:
    """Tests for boss scene generation."""

    def test_encounter_returns_valid_png(self):
        buf = draw_boss_scene("Dirt")
        img = Image.open(buf)
        assert img.format == "PNG"
        assert img.size == (320, 180)

    def test_result_victory_returns_valid_png(self):
        buf = draw_boss_result_scene("Crystal", won=True)
        img = Image.open(buf)
        assert img.format == "PNG"

    def test_result_defeat_returns_valid_png(self):
        buf = draw_boss_result_scene("Magma", won=False)
        img = Image.open(buf)
        assert img.format == "PNG"


class TestPinnaclePhaseDrawing:
    def test_phase2_is_rgba_png_within_runtime_limit(self):
        buf = dig_drawing.draw_pinnacle_phase2(
            _encounter_png_bytes(), "forgotten_king",
        )

        assert isinstance(buf, io.BytesIO)
        assert len(buf.getvalue()) <= 512 * 1024
        with Image.open(buf) as image:
            assert image.format == "PNG"
            assert image.size == (512, 288)
            assert image.mode == "RGBA"

    def test_phase3_is_eight_frame_play_once_gif_with_final_hold(self):
        buf = dig_drawing.animate_pinnacle_phase3(
            _encounter_png_bytes(), "hollowforged",
        )

        assert isinstance(buf, io.BytesIO)
        assert len(buf.getvalue()) <= 750 * 1024
        with Image.open(buf) as image:
            assert image.format == "GIF"
            assert image.size == (512, 288)
            assert image.n_frames == 8
            assert image.info.get("loop") is None
            durations = []
            for frame_idx in range(image.n_frames):
                image.seek(frame_idx)
                durations.append(image.info["duration"])
            assert durations[-1] >= 1_000
            assert durations[-1] > max(durations[:-1])

    def test_all_pinnacle_themes_and_secret_palette_are_distinct(self):
        source = _encounter_png_bytes()
        normal_outputs = {
            boss_id: dig_drawing.draw_pinnacle_phase2(source, boss_id).getvalue()
            for boss_id in PINNACLE_BOSS_IDS
        }

        assert set(dig_drawing.PINNACLE_PHASE_THEMES) == set(PINNACLE_BOSS_IDS)
        assert len(set(normal_outputs.values())) == len(PINNACLE_BOSS_IDS)

    @pytest.mark.parametrize("boss_id", PINNACLE_BOSS_IDS)
    def test_secret_palette_changes_both_pinnacle_phases(self, boss_id):
        normal_phase2, normal_phase3 = _pinnacle_phases(boss_id, False)
        secret_phase2, secret_phase3 = _pinnacle_phases(boss_id, True)

        assert normal_phase2 != secret_phase2
        assert normal_phase3 != secret_phase3

    @pytest.mark.parametrize("boss_id", PINNACLE_BOSS_IDS)
    @pytest.mark.parametrize("secret", (False, True), ids=("normal", "secret"))
    def test_pinnacle_effects_preserve_the_source_art(self, boss_id, secret):
        source = (
            ASSETS_DIR / "bosses" / f"{boss_id}_encounter.png"
        ).read_bytes()
        with Image.open(io.BytesIO(source)) as source_image:
            reference = source_image.convert("RGB").resize(
                (512, 288),
                Image.Resampling.LANCZOS,
            )

        phase2_bytes, phase3_bytes = _pinnacle_phases(boss_id, secret)
        assert len(phase2_bytes) <= 512 * 1024
        with Image.open(io.BytesIO(phase2_bytes)) as phase2_image:
            phase2 = phase2_image.convert("RGB")
        assert (
            _strongly_changed_fraction(phase2, reference)
            < MAX_STRONGLY_CHANGED_FRACTION
        )

        assert len(phase3_bytes) <= 750 * 1024
        with Image.open(io.BytesIO(phase3_bytes)) as phase3:
            assert phase3.n_frames == 8
            for frame_index in range(phase3.n_frames):
                phase3.seek(frame_index)
                assert (
                    _strongly_changed_fraction(
                        phase3.convert("RGB"),
                        reference,
                    )
                    < MAX_STRONGLY_CHANGED_FRACTION
                )

    @pytest.mark.parametrize(
        ("boss_id", "secret"),
        [("forgotten_king", False), ("lantern_engine", True)],
        ids=("normal", "secret"),
    )
    def test_pinnacle_phase3_effects_are_deterministic(self, boss_id, secret):
        """A fresh render is byte-identical to the shared cached render.

        Determinism comes from the seeded effect pipeline shared by every
        theme, so one normal and one secret case cover it; repeating the
        double-render for all 6 bosses x 2 palettes added no coverage.
        """
        source = (
            ASSETS_DIR / "bosses" / f"{boss_id}_encounter.png"
        ).read_bytes()

        fresh = dig_drawing.animate_pinnacle_phase3(
            source,
            boss_id,
            secret=secret,
        )

        assert fresh.getvalue() == _pinnacle_phases(boss_id, secret)[1]


class TestDrawEventScene:
    """Tests for event scene generation."""

    def test_known_event_returns_valid_png(self):
        buf = draw_event_scene("Dirt", "pudge_fishing")
        img = Image.open(buf)
        assert img.format == "PNG"
        assert img.size == (320, 180)

    def test_unknown_event_still_renders(self):
        """Unknown event_id produces a scene with just the background and player."""
        buf = draw_event_scene("Stone", "nonexistent_event")
        img = Image.open(buf)
        assert img.size == (320, 180)


class TestHasEventScene:
    """Tests for event scene registry lookup."""

    def test_known_event(self):
        assert has_event_scene("pudge_fishing") is True

    def test_unknown_event(self):
        assert has_event_scene("definitely_not_a_real_event") is False


# =============================================================================
# dig_assets tests
# =============================================================================


class TestFindAsset:
    """Tests for asset file discovery."""

    def test_finds_png_file(self, tmp_path):
        (tmp_path / "test.png").write_bytes(b"\x89PNG" + b"\x00" * 100)
        result = _find_asset(tmp_path, "test")
        assert result is not None
        assert result.name == "test.png"

    def test_returns_none_for_missing(self, tmp_path):
        result = _find_asset(tmp_path, "nonexistent")
        assert result is None

    def test_skips_oversized_file(self, tmp_path):
        big = tmp_path / "big.png"
        big.write_bytes(b"\x00" * (_MAX_FILE_SIZE + 1))
        result = _find_asset(tmp_path, "big")
        assert result is None

    def test_prefers_gif_over_png(self, tmp_path):
        (tmp_path / "test.gif").write_bytes(b"GIF89a" + b"\x00" * 50)
        (tmp_path / "test.png").write_bytes(b"\x89PNG" + b"\x00" * 50)
        result = _find_asset(tmp_path, "test")
        assert result.suffix == ".gif"


class TestLoadCachedBytes:
    """Tests for byte-level caching."""

    def test_loads_and_caches(self, tmp_path):
        f = tmp_path / "data.bin"
        f.write_bytes(b"hello")
        data = _load_cached_bytes(f)
        assert data == b"hello"
        # Second call returns cached value
        f.unlink()
        data2 = _load_cached_bytes(f)
        assert data2 == b"hello"

    def test_returns_none_for_missing_file(self, tmp_path):
        result = _load_cached_bytes(tmp_path / "nope.bin")
        assert result is None


class TestGetBossArt:
    """Tests for boss art loading with fallback chain."""

    def test_resolves_grandfathered_boss_by_id(self):
        """Grandfathered boss slug resolves on-disk PNG or PIL fallback."""
        result = get_boss_art("grothak", "encounter", "Dirt")
        assert result is not None
        assert hasattr(result, "filename")

    def test_resolves_dota_boss_by_id(self):
        """New Dota boss_ids resolve their per-boss slug."""
        result = get_boss_art("pudge", "encounter", "Dirt")
        assert result is not None
        assert hasattr(result, "filename")

    def test_legacy_depth_int_still_resolves(self):
        """Backward-compat: an int boundary routes to the grandfathered boss."""
        result = get_boss_art(25, "encounter", "Dirt")
        assert result is not None
        assert hasattr(result, "filename")

    def test_returns_none_for_unknown_boss(self):
        assert get_boss_art("not_a_boss", "encounter", "Dirt") is None
        assert get_boss_art(9999, "encounter", "Dirt") is None

    def test_every_registered_boss_id_has_a_slug(self):
        """Every boss in BOSSES_BY_ID must be resolvable by get_boss_art.

        Catches the case where a new BossDef is added but its boss_id is
        omitted from BOSS_SLUGS — without this check, the asset path
        silently returns None instead of falling through to PIL.
        """
        from services.dig_constants import BOSSES_BY_ID
        from utils.dig_assets import BOSS_SLUGS
        missing = [bid for bid in BOSSES_BY_ID if bid not in BOSS_SLUGS]
        assert not missing, f"boss_ids missing from BOSS_SLUGS: {missing}"


class TestGetPinnaclePhaseArt:
    def _write_encounter(self, tmp_path, boss_id="forgotten_king"):
        boss_dir = tmp_path / "bosses"
        boss_dir.mkdir()
        path = boss_dir / f"{boss_id}_encounter.png"
        path.write_bytes(_encounter_png_bytes())
        return path

    def test_caches_generated_bytes_but_returns_fresh_files(
        self, monkeypatch, tmp_path,
    ):
        self._write_encounter(tmp_path)
        monkeypatch.setattr(dig_assets, "ASSETS_DIR", tmp_path)
        _clear_pinnacle_phase_state()
        calls = []

        def generate(source, boss_id, *, secret=False):
            calls.append((source, boss_id, secret))
            return io.BytesIO(_encounter_png_bytes((120, 20, 20, 255)))

        monkeypatch.setattr(dig_drawing, "draw_pinnacle_phase2", generate)

        async def scenario():
            first = await dig_assets.get_pinnacle_phase_art(
                "forgotten_king", 2, "Abyss",
            )
            second = await dig_assets.get_pinnacle_phase_art(
                "forgotten_king", 2, "Abyss",
            )
            assert first is not None and second is not None
            assert first is not second
            assert first.fp is not second.fp
            assert first.fp.read() == second.fp.read()

        asyncio.run(scenario())
        assert len(calls) == 1

    def test_concurrent_misses_coalesce_and_return_fresh_files(self, monkeypatch):
        _clear_pinnacle_phase_state()
        started = threading.Event()
        release = threading.Event()
        calls = []
        rendered = _encounter_png_bytes((120, 20, 20, 255))

        def generate(*args, **kwargs):
            calls.append((args, kwargs))
            started.set()
            assert release.wait(timeout=2)
            return rendered

        monkeypatch.setattr(dig_assets, "_generate_pinnacle_phase_art", generate)

        async def scenario():
            first_task = asyncio.create_task(
                dig_assets.get_pinnacle_phase_art(
                    "forgotten_king", 2, "Abyss",
                ),
            )
            assert await asyncio.to_thread(started.wait, 2)
            second_task = asyncio.create_task(
                dig_assets.get_pinnacle_phase_art(
                    "forgotten_king", 2, "Abyss",
                ),
            )
            await asyncio.sleep(0)
            release.set()
            first, second = await asyncio.gather(first_task, second_task)
            assert first is not None and second is not None
            assert first is not second
            assert first.fp is not second.fp
            assert first.fp.read() == second.fp.read() == rendered

        asyncio.run(scenario())
        assert len(calls) == 1
        assert not dig_assets._pinnacle_phase_art_inflight

    def test_cancelled_waiter_does_not_cancel_shared_render(self, monkeypatch):
        _clear_pinnacle_phase_state()
        started = threading.Event()
        release = threading.Event()
        calls = []
        rendered = _encounter_png_bytes((120, 20, 20, 255))

        def generate(*args, **kwargs):
            calls.append((args, kwargs))
            started.set()
            assert release.wait(timeout=2)
            return rendered

        monkeypatch.setattr(dig_assets, "_generate_pinnacle_phase_art", generate)

        async def scenario():
            cancelled_waiter = asyncio.create_task(
                dig_assets.get_pinnacle_phase_art(
                    "forgotten_king", 2, "Abyss",
                ),
            )
            assert await asyncio.to_thread(started.wait, 2)
            surviving_waiter = asyncio.create_task(
                dig_assets.get_pinnacle_phase_art(
                    "forgotten_king", 2, "Abyss",
                ),
            )
            await asyncio.sleep(0)
            cancelled_waiter.cancel()
            with pytest.raises(asyncio.CancelledError):
                await cancelled_waiter
            release.set()
            art = await surviving_waiter
            assert art is not None
            assert art.fp.read() == rendered

        asyncio.run(scenario())
        assert len(calls) == 1
        assert not dig_assets._pinnacle_phase_art_inflight

    def test_missing_phase_files_are_not_required(self, monkeypatch, tmp_path):
        self._write_encounter(tmp_path)
        monkeypatch.setattr(dig_assets, "ASSETS_DIR", tmp_path)
        _clear_pinnacle_phase_state()

        async def scenario():
            art = await dig_assets.get_pinnacle_phase_art(
                "forgotten_king", 3, "Abyss",
            )
            assert art is not None
            assert art.filename.endswith(".gif")

        asyncio.run(scenario())
        assert not list((tmp_path / "bosses").glob("*phase*"))
        assert not list((tmp_path / "bosses").glob("*.gif"))

    def test_generation_failure_falls_back_to_static_encounter(
        self, monkeypatch, tmp_path,
    ):
        source_path = self._write_encounter(tmp_path)
        monkeypatch.setattr(dig_assets, "ASSETS_DIR", tmp_path)
        _clear_pinnacle_phase_state()

        def fail(*args, **kwargs):
            raise RuntimeError("render failed")

        monkeypatch.setattr(dig_drawing, "draw_pinnacle_phase2", fail)

        async def scenario():
            art = await dig_assets.get_pinnacle_phase_art(
                "forgotten_king", 2, "Abyss",
            )
            assert art is not None
            assert art.filename == "boss_encounter.png"
            assert art.fp.read() == source_path.read_bytes()

        asyncio.run(scenario())

    def test_missing_encounter_asset_falls_back_to_static_art(
        self, monkeypatch, tmp_path,
    ):
        (tmp_path / "bosses").mkdir()
        monkeypatch.setattr(dig_assets, "ASSETS_DIR", tmp_path)
        _clear_pinnacle_phase_state()

        async def scenario():
            art = await dig_assets.get_pinnacle_phase_art(
                "forgotten_king", 2, "Abyss",
            )
            assert art is not None
            assert art.filename == "boss_encounter.png"

        asyncio.run(scenario())

    def test_oversized_generation_falls_back_to_static_encounter(
        self, monkeypatch, tmp_path,
    ):
        source_path = self._write_encounter(tmp_path)
        monkeypatch.setattr(dig_assets, "ASSETS_DIR", tmp_path)
        _clear_pinnacle_phase_state()
        monkeypatch.setattr(
            dig_drawing,
            "animate_pinnacle_phase3",
            MagicMock(return_value=io.BytesIO(b"x" * (750 * 1024 + 1))),
        )

        async def scenario():
            art = await dig_assets.get_pinnacle_phase_art(
                "forgotten_king", 3, "Abyss",
            )
            assert art is not None
            assert art.filename == "boss_encounter.png"
            assert art.fp.read() == source_path.read_bytes()

        asyncio.run(scenario())

    @pytest.mark.parametrize("outcome", ["failure", "oversize"])
    def test_failed_generation_is_negatively_cached(self, monkeypatch, outcome):
        _clear_pinnacle_phase_state()
        rendered = (
            b"x" * (750 * 1024 + 1)
            if outcome == "oversize"
            else None
        )
        generate = MagicMock(
            return_value=rendered,
            side_effect=RuntimeError("render failed") if outcome == "failure" else None,
        )
        monkeypatch.setattr(
            dig_assets, "_generate_pinnacle_phase_art", generate,
        )
        static_calls = []

        def static_art(*args, **kwargs):
            static_calls.append((args, kwargs))
            return discord.File(
                io.BytesIO(_encounter_png_bytes()), filename="boss_encounter.png",
            )

        monkeypatch.setattr(dig_assets, "get_boss_art", static_art)

        async def scenario():
            first = await dig_assets.get_pinnacle_phase_art(
                "forgotten_king", 3, "Abyss",
            )
            second = await dig_assets.get_pinnacle_phase_art(
                "forgotten_king", 3, "Abyss",
            )
            assert first is not None and second is not None
            assert first is not second

        asyncio.run(scenario())
        assert generate.call_count == 1
        assert len(static_calls) == 2
        assert ("forgotten_king", 3, False) in (
            dig_assets._pinnacle_phase_art_failures
        )
        assert not dig_assets._pinnacle_phase_art_inflight


class TestPrestige2BossAssets:
    """The new boss illustrations match the established asset contract."""

    def test_assets_are_consistently_sized_rgba_pngs(self):
        for boss_id in NEW_PRESTIGE2_BOSS_ASSET_IDS:
            for state in BOSS_ART_STATES:
                path = ASSETS_DIR / "bosses" / f"{boss_id}_{state}.png"
                assert path.is_file(), f"missing boss asset: {path}"
                with Image.open(path) as image:
                    assert image.format == "PNG"
                    assert image.size == (512, 288)
                    assert image.mode == "RGBA"

    def test_boss_loader_resolves_every_new_asset(self):
        for boss_id in NEW_PRESTIGE2_BOSS_ASSET_IDS:
            for state in BOSS_ART_STATES:
                path = ASSETS_DIR / "bosses" / f"{boss_id}_{state}.png"
                asset = get_boss_art(boss_id, state, "Stone")
                assert asset is not None
                assert asset.filename == f"boss_{state}.png"
                assert asset.fp.read() == path.read_bytes()


class TestPinnacleBossAssets:
    """Pinnacle source art stays compact and compatible with phase rendering."""

    def test_new_assets_are_compact_rgba_pngs(self):
        new_boss_ids = ("bastion_below", "pale_surveyor", "lantern_engine")
        paths = [
            ASSETS_DIR / "bosses" / f"{boss_id}_{state}.png"
            for boss_id in new_boss_ids
            for state in BOSS_ART_STATES
        ]

        for path in paths:
            assert path.is_file(), f"missing boss asset: {path}"
            assert path.stat().st_size <= 512 * 1024
            with Image.open(path) as image:
                assert image.format == "PNG"
                assert image.size == (512, 288)
                assert image.mode == "RGBA"

        assert sum(path.stat().st_size for path in paths) <= 4 * 1024 * 1024

    def test_new_boss_loader_returns_each_source_asset(self):
        for boss_id in NEW_PINNACLE_BOSS_ASSET_IDS:
            for state in BOSS_ART_STATES:
                path = ASSETS_DIR / "bosses" / f"{boss_id}_{state}.png"
                asset = get_boss_art(boss_id, state, "The Hollow")
                assert asset is not None
                assert asset.filename == f"boss_{state}.png"
                assert asset.fp.read() == path.read_bytes()


class TestGetLayerThumbnail:
    """Tests for layer thumbnail loading."""

    def test_returns_file_for_known_layer(self):
        result = get_layer_thumbnail("Dirt")
        assert result is not None
        assert hasattr(result, "filename")

    def test_returns_none_for_unknown_layer(self):
        result = get_layer_thumbnail("Nonexistent Layer")
        assert result is None


ROGUELIKE_EVENT_ASSET_IDS = (
    "collapsed_armory",
    "dead_prospectors_pack",
    "spore_debt",
    "clockwork_toll",
    "collectors_roots",
    "hungry_beacon",
    "abandoned_forge",
    "salted_threshold",
    "frayed_lifeline",
    "quartermaster_s_niche",
    "collapsed_armory_cache",
    "relic_bearing_strata",
    "prospector_s_last_pack",
    "song_below_stone",
    "first_descent_cartography",
    "surveyors_cairn",
    "ruinwager_vault",
    "red_thread_shrine",
)


class TestRoguelikeEventAssets:
    """The new event illustrations match the established asset contract."""

    def test_assets_are_consistently_sized_rgb_pngs(self):
        for event_id in ROGUELIKE_EVENT_ASSET_IDS:
            path = ASSETS_DIR / "events" / f"{event_id}.png"
            assert path.is_file(), f"missing event asset: {path}"
            with Image.open(path) as image:
                assert image.format == "PNG"
                assert image.size == (1024, 576)
                assert image.mode == "RGB"

    def test_event_loader_resolves_every_new_asset(self):
        for event_id in ROGUELIKE_EVENT_ASSET_IDS:
            asset = get_event_art(event_id, "Stone")
            assert asset is not None
            assert asset.filename == f"event_{event_id}.png"


PRESSURE_EVENT_ASSET_IDS = (
    "second_life_ledger",
    "twin_scar_altar",
    "ember_clearinghouse",
)


class TestPressureEventAssets:
    def test_assets_are_consistently_sized_rgb_pngs(self):
        for event_id in PRESSURE_EVENT_ASSET_IDS:
            path = ASSETS_DIR / "events" / f"{event_id}.png"
            assert path.is_file(), f"missing event asset: {path}"
            with Image.open(path) as image:
                assert image.format == "PNG"
                assert image.size == (1024, 576)
                assert image.mode == "RGB"

    def test_event_loader_resolves_every_new_asset(self):
        for event_id in PRESSURE_EVENT_ASSET_IDS:
            asset = get_event_art(event_id, "Stone")
            assert asset is not None
            assert asset.filename == f"event_{event_id}.png"
