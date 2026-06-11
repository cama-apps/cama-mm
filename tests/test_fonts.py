"""Tests for the shared cached font loader (utils/fonts.py)."""

import pytest
from PIL import ImageFont

from utils import fonts


class _FakeFont:
    """Stand-in for a loaded truetype font, recording what was requested."""

    def __init__(self, path: str, size: int):
        self.path = path
        self.size = size


@pytest.fixture(autouse=True)
def fresh_cache(monkeypatch):
    """Isolate each test from the module-level font cache."""
    monkeypatch.setattr(fonts, "_FONT_CACHE", {})


@pytest.fixture
def fake_truetype(monkeypatch):
    """Replace ImageFont.truetype with a recorder that always succeeds."""
    monkeypatch.setattr(
        fonts.ImageFont, "truetype", lambda path, size: _FakeFont(path, size)
    )


def test_repeated_calls_return_same_object():
    assert fonts.get_font(16) is fonts.get_font(16)
    assert fonts.get_font(14, bold=True) is fonts.get_font(14, bold=True)
    assert fonts.get_font(12, mono=True) is fonts.get_font(12, mono=True)


def test_distinct_params_get_distinct_entries(fake_truetype):
    base = fonts.get_font(16)
    assert fonts.get_font(18) is not base
    assert fonts.get_font(16, bold=True) is not base
    assert fonts.get_font(16, mono=True) is not base
    assert fonts.get_font(16, bold=True, mono=True) is not fonts.get_font(
        16, bold=True
    )


def test_font_file_resolution(fake_truetype):
    """Each style resolves to the same DejaVu file the old helpers loaded."""
    assert fonts.get_font(16).path.endswith("/DejaVuSans.ttf")
    assert fonts.get_font(16, bold=True).path.endswith("/DejaVuSans-Bold.ttf")
    assert fonts.get_font(16, mono=True).path.endswith("/DejaVuSansMono.ttf")
    assert fonts.get_font(16, bold=True, mono=True).path.endswith(
        "/DejaVuSansMono-Bold.ttf"
    )
    assert fonts.get_font(16).size == 16


def test_mono_falls_back_to_dejavu_sans(monkeypatch):
    """Missing mono fonts fall through to DejaVuSans, as neon_drawing did."""

    def truetype(path, size):
        if "Mono" in path:
            raise OSError("mono not installed")
        return _FakeFont(path, size)

    monkeypatch.setattr(fonts.ImageFont, "truetype", truetype)
    assert fonts.get_font(16, mono=True).path.endswith("/DejaVuSans.ttf")
    assert fonts.get_font(16, bold=True, mono=True).path.endswith(
        "/DejaVuSans-Bold.ttf"
    )


def test_total_truetype_failure_returns_default(monkeypatch):
    """If every truetype load raises OSError, load_default is returned."""
    real_truetype = ImageFont.truetype

    def boom(font=None, size=10, *args, **kwargs):
        if isinstance(font, str):
            raise OSError("no fonts anywhere")
        # load_default() loads its embedded font via truetype(BytesIO(...)).
        return real_truetype(font, size, *args, **kwargs)

    monkeypatch.setattr(fonts.ImageFont, "truetype", boom)
    for kwargs in (
        {},
        {"bold": True},
        {"mono": True},
        {"bold": True, "mono": True},
    ):
        font = fonts.get_font(20, **kwargs)
        assert isinstance(font, type(ImageFont.load_default()))
