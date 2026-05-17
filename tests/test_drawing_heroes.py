"""
Render smoke tests for hero-grid drawing.

These guard behavior-preservation for refactors of `draw_hero_grid`: the
function must always return a valid, non-empty, seekable PNG BytesIO whose
content begins with the PNG signature, for both populated and empty input.
"""

from io import BytesIO

from PIL import Image

from utils.drawing import draw_hero_grid

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def _assert_valid_png(result):
    """Assert result is a seekable, non-empty PNG BytesIO."""
    assert isinstance(result, BytesIO)
    # Returned at start, seekable.
    assert result.tell() == 0
    header = result.read(8)
    assert header == PNG_SIGNATURE
    result.seek(0)
    # Non-empty payload.
    payload = result.read()
    assert len(payload) > len(PNG_SIGNATURE)
    result.seek(0)
    # Decodes as a real PNG with positive dimensions.
    img = Image.open(result)
    assert img.format == "PNG"
    assert img.size[0] > 0 and img.size[1] > 0


class TestDrawHeroGridSmoke:
    """Smoke tests covering the populated and empty render paths."""

    def test_populated_input_returns_valid_png(self):
        """A grid with multiple players and heroes renders a valid PNG."""
        grid_data = [
            {"discord_id": 1, "hero_id": 1, "games": 12, "wins": 9},
            {"discord_id": 1, "hero_id": 5, "games": 6, "wins": 2},
            {"discord_id": 2, "hero_id": 1, "games": 8, "wins": 4},
            {"discord_id": 2, "hero_id": 11, "games": 4, "wins": 1},
            {"discord_id": 3, "hero_id": 5, "games": 10, "wins": 7},
        ]
        player_names = {1: "Alice", 2: "Bob", 3: "Carol"}
        result = draw_hero_grid(grid_data, player_names, min_games=2, title="Smoke")
        _assert_valid_png(result)

    def test_empty_input_returns_valid_png(self):
        """Empty grid data and player names still render a valid PNG."""
        result = draw_hero_grid([], {})
        _assert_valid_png(result)

    def test_minimal_input_returns_valid_png(self):
        """A single player on a single hero renders a valid PNG."""
        grid_data = [{"discord_id": 1, "hero_id": 1, "games": 3, "wins": 2}]
        player_names = {1: "Solo"}
        result = draw_hero_grid(grid_data, player_names, min_games=1)
        _assert_valid_png(result)
