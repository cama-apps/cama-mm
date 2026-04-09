"""Tests for dig minigame asset loading and pixel art generation."""

import io

from PIL import Image

from utils.dig_assets import (
    _MAX_FILE_SIZE,
    _find_asset,
    _load_cached_bytes,
    get_boss_art,
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
        buf = draw_boss_scene("Dirt", "grothak")
        img = Image.open(buf)
        assert img.format == "PNG"
        assert img.size == (320, 180)

    def test_result_victory_returns_valid_png(self):
        buf = draw_boss_result_scene("Crystal", "crystalia", won=True)
        img = Image.open(buf)
        assert img.format == "PNG"

    def test_result_defeat_returns_valid_png(self):
        buf = draw_boss_result_scene("Magma", "magmus", won=False)
        img = Image.open(buf)
        assert img.format == "PNG"


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

    def test_falls_back_to_pil_when_no_file(self):
        """With no custom art on disk for boundary 9999, falls back to PIL."""
        result = get_boss_art(25, "encounter", "Dirt")
        # Should return a discord.File (PIL fallback) or None
        # Since PIL is available and "grothak" is a valid slug for boundary 25,
        # this should succeed
        assert result is not None
        assert hasattr(result, "filename")

    def test_returns_none_for_unknown_boundary(self):
        result = get_boss_art(9999, "encounter", "Dirt")
        assert result is None


class TestGetLayerThumbnail:
    """Tests for layer thumbnail loading."""

    def test_returns_file_for_known_layer(self):
        result = get_layer_thumbnail("Dirt")
        assert result is not None
        assert hasattr(result, "filename")

    def test_returns_none_for_unknown_layer(self):
        result = get_layer_thumbnail("Nonexistent Layer")
        assert result is None
