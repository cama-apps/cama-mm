"""Coverage for JOPA-T's themed post-match settlement animations."""

import io

import pytest
from PIL import Image

from utils.neon_drawing import POST_MATCH_GIF_THEMES, create_post_match_gif

EXPECTED_THEMES = (
    "divine_rapier_position",
    "buyback_denied",
    "ancient_liquidated",
    "beyond_godlike",
    "odds_anomaly",
)


@pytest.mark.parametrize("theme", EXPECTED_THEMES)
def test_post_match_gif_theme_is_seekable_animated_gif(theme):
    """Every advertised theme renders a 400x300 multi-frame Pillow GIF."""
    buffer = create_post_match_gif("Very Long Display Name " * 3, 1234567, theme=theme)

    assert isinstance(buffer, io.BytesIO)
    assert buffer.tell() == 0
    assert buffer.getbuffer().nbytes < 4 * 1024 * 1024
    buffer.seek(0)
    with Image.open(buffer) as image:
        assert image.format == "GIF"
        assert image.size == (400, 300)
        assert image.n_frames > 1
        for frame_index in range(image.n_frames):
            image.seek(frame_index)
            image.load()


def test_post_match_gif_exports_all_supported_themes():
    """The selection list stays aligned with the post-match integrations."""
    assert POST_MATCH_GIF_THEMES == EXPECTED_THEMES


def test_post_match_gif_rejects_unknown_theme():
    """Callers receive a clear error rather than an unthemed animation."""
    with pytest.raises(ValueError, match="Unsupported post-match GIF theme"):
        create_post_match_gif("TestUser", 100, theme="not_a_theme")
