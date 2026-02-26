"""
Tests for drawing utilities.
"""

from io import BytesIO

from PIL import Image

from utils.drawing import (
    draw_attribute_distribution,
    draw_lane_distribution,
    draw_matches_table,
    draw_rating_history_chart,
    draw_role_graph,
)


class TestDrawMatchesTable:
    """Tests for draw_matches_table function."""

    def test_empty_matches_returns_image(self):
        """Test that empty matches list returns valid image."""
        result = draw_matches_table([])
        assert isinstance(result, BytesIO)

        # Verify it's a valid PNG
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size[0] > 0 and img.size[1] > 0

    def test_single_match(self):
        """Test table with single match."""
        matches = [
            {
                "hero_name": "Anti-Mage",
                "kills": 12,
                "deaths": 3,
                "assists": 8,
                "won": True,
                "duration": 2400,
            }
        ]
        result = draw_matches_table(matches)

        img = Image.open(result)
        assert img.format == "PNG"
        # Should have reasonable dimensions
        assert img.size[0] >= 300
        assert img.size[1] >= 50

    def test_multiple_matches(self):
        """Test table with multiple matches."""
        matches = [
            {
                "hero_name": "Pudge",
                "kills": 5,
                "deaths": 10,
                "assists": 15,
                "won": False,
                "duration": 1800,
            },
            {
                "hero_name": "Crystal Maiden",
                "kills": 2,
                "deaths": 8,
                "assists": 25,
                "won": True,
                "duration": 3000,
            },
            {
                "hero_name": "Axe",
                "kills": 8,
                "deaths": 5,
                "assists": 12,
                "won": True,
                "duration": 2100,
            },
        ]
        result = draw_matches_table(matches)

        img = Image.open(result)
        assert img.format == "PNG"
        # More matches should mean taller image
        assert img.size[1] >= 100

    def test_hero_id_with_names_dict(self):
        """Test using hero_id with hero_names dict."""
        matches = [
            {"hero_id": 1, "kills": 10, "deaths": 2, "assists": 5, "won": True, "duration": 2000},
        ]
        hero_names = {1: "Anti-Mage", 2: "Axe"}

        result = draw_matches_table(matches, hero_names=hero_names)

        img = Image.open(result)
        assert img.format == "PNG"

    def test_missing_duration(self):
        """Test handling of missing duration."""
        matches = [
            {"hero_name": "Pudge", "kills": 5, "deaths": 10, "assists": 15, "won": False},
        ]
        result = draw_matches_table(matches)

        img = Image.open(result)
        assert img.format == "PNG"


class TestDrawRoleGraph:
    """Tests for draw_role_graph function."""

    def test_basic_role_graph(self):
        """Test basic role graph generation."""
        roles = {
            "Carry": 30.0,
            "Support": 25.0,
            "Nuker": 20.0,
            "Disabler": 15.0,
            "Initiator": 10.0,
        }
        result = draw_role_graph(roles)

        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size == (400, 400)

    def test_role_graph_with_title(self):
        """Test role graph with custom title."""
        roles = {"Carry": 50.0, "Support": 50.0, "Nuker": 0.0}
        result = draw_role_graph(roles, title="My Roles")

        img = Image.open(result)
        assert img.format == "PNG"

    def test_role_graph_few_roles(self):
        """Test role graph with only 2 roles (should show message)."""
        roles = {"Carry": 60.0, "Support": 40.0}
        result = draw_role_graph(roles)

        img = Image.open(result)
        assert img.format == "PNG"

    def test_role_graph_many_roles(self):
        """Test role graph with many roles."""
        roles = {
            "Carry": 20.0,
            "Nuker": 15.0,
            "Initiator": 12.0,
            "Disabler": 10.0,
            "Durable": 10.0,
            "Escape": 10.0,
            "Support": 13.0,
            "Pusher": 5.0,
            "Jungler": 5.0,
        }
        result = draw_role_graph(roles)

        img = Image.open(result)
        assert img.format == "PNG"


class TestDrawLaneDistribution:
    """Tests for draw_lane_distribution function."""

    def test_basic_lane_distribution(self):
        """Test basic lane distribution bar chart."""
        lanes = {
            "Safe Lane": 40.0,
            "Mid": 25.0,
            "Off Lane": 30.0,
            "Jungle": 5.0,
        }
        result = draw_lane_distribution(lanes)

        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size[0] == 350

    def test_lane_distribution_all_zeros(self):
        """Test lane distribution with all zeros."""
        lanes = {
            "Safe Lane": 0,
            "Mid": 0,
            "Off Lane": 0,
            "Jungle": 0,
        }
        result = draw_lane_distribution(lanes)

        img = Image.open(result)
        assert img.format == "PNG"

    def test_lane_distribution_single_lane(self):
        """Test lane distribution with only one lane."""
        lanes = {"Mid": 100.0}
        result = draw_lane_distribution(lanes)

        img = Image.open(result)
        assert img.format == "PNG"


class TestDrawAttributeDistribution:
    """Tests for draw_attribute_distribution function."""

    def test_basic_attribute_distribution(self):
        """Test basic attribute pie chart."""
        attrs = {
            "str": 25.0,
            "agi": 35.0,
            "int": 30.0,
            "all": 10.0,
        }
        result = draw_attribute_distribution(attrs)

        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size == (300, 300)

    def test_attribute_distribution_missing_attrs(self):
        """Test with some attributes at zero."""
        attrs = {
            "str": 0,
            "agi": 60.0,
            "int": 40.0,
            "all": 0,
        }
        result = draw_attribute_distribution(attrs)

        img = Image.open(result)
        assert img.format == "PNG"

    def test_attribute_distribution_single_attr(self):
        """Test with single attribute dominating."""
        attrs = {
            "str": 100.0,
            "agi": 0,
            "int": 0,
            "all": 0,
        }
        result = draw_attribute_distribution(attrs)

        img = Image.open(result)
        assert img.format == "PNG"


class TestImageIntegrity:
    """Tests for image integrity and format."""

    def test_all_functions_return_seekable_bytesio(self):
        """Test that all drawing functions return seekable BytesIO."""
        funcs = [
            (
                draw_matches_table,
                [
                    [
                        {
                            "hero_name": "Pudge",
                            "kills": 1,
                            "deaths": 2,
                            "assists": 3,
                            "won": True,
                            "duration": 1000,
                        }
                    ]
                ],
            ),
            (draw_role_graph, [{"Carry": 50.0, "Support": 30.0, "Nuker": 20.0}]),
            (draw_lane_distribution, [{"Safe Lane": 50.0, "Mid": 50.0}]),
            (draw_attribute_distribution, [{"str": 50.0, "agi": 50.0}]),
        ]

        for func, args in funcs:
            result = func(*args)
            assert isinstance(result, BytesIO)
            assert result.tell() == 0  # Should be at start
            # Should be seekable
            result.seek(10)
            result.seek(0)

    def test_all_images_are_rgba(self):
        """Test that all generated images use RGBA mode."""
        funcs = [
            (
                draw_matches_table,
                [
                    [
                        {
                            "hero_name": "Test",
                            "kills": 1,
                            "deaths": 1,
                            "assists": 1,
                            "won": True,
                            "duration": 100,
                        }
                    ]
                ],
            ),
            (draw_role_graph, [{"A": 33.0, "B": 33.0, "C": 34.0}]),
            (draw_lane_distribution, [{"Safe Lane": 100.0}]),
            (draw_attribute_distribution, [{"str": 100.0}]),
        ]

        for func, args in funcs:
            result = func(*args)
            img = Image.open(result)
            assert img.mode == "RGBA"


def _make_history_entry(rating=1500, won=True, os_mu=None):
    """Helper to create a rating history dict."""
    return {
        "rating": rating,
        "rating_before": rating - (50 if won else -50),
        "rd_before": 100,
        "rd_after": 95,
        "volatility_before": 0.06,
        "volatility_after": 0.06,
        "expected_team_win_prob": 0.5,
        "team_number": 1,
        "won": won,
        "match_id": 1,
        "timestamp": "2025-01-01T00:00:00",
        "lobby_type": "shuffle",
        "os_mu_before": os_mu - 1 if os_mu else None,
        "os_mu_after": os_mu,
        "os_sigma_before": 8.0 if os_mu else None,
        "os_sigma_after": 7.5 if os_mu else None,
    }


class TestDrawRatingHistoryChart:
    """Tests for draw_rating_history_chart function."""

    def test_empty_history_returns_image(self):
        """Test that empty history returns valid image with message."""
        result = draw_rating_history_chart("TestUser", [])
        assert isinstance(result, BytesIO)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size == (700, 400)

    def test_basic_chart_with_both_ratings(self):
        """Test chart with both Glicko and OpenSkill data."""
        history = [
            _make_history_entry(rating=1600, won=True, os_mu=40),
            _make_history_entry(rating=1550, won=False, os_mu=38),
            _make_history_entry(rating=1500, won=True, os_mu=35),
        ]
        result = draw_rating_history_chart("TestUser", history)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size == (700, 400)
        assert img.mode == "RGBA"

    def test_chart_without_openskill(self):
        """Test chart with only Glicko data (no OpenSkill)."""
        history = [
            _make_history_entry(rating=1600, won=True),
            _make_history_entry(rating=1550, won=False),
            _make_history_entry(rating=1500, won=True),
        ]
        result = draw_rating_history_chart("TestUser", history)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size == (700, 400)

    def test_two_matches_minimum(self):
        """Test that exactly 2 matches produces a valid chart."""
        history = [
            _make_history_entry(rating=1550, won=True, os_mu=40),
            _make_history_entry(rating=1500, won=False, os_mu=35),
        ]
        result = draw_rating_history_chart("TestUser", history)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size == (700, 400)

    def test_large_history(self):
        """Test chart with 50+ matches."""
        history = [
            _make_history_entry(
                rating=1500 + i * 10,
                won=i % 3 != 0,
                os_mu=30 + i * 0.5,
            )
            for i in range(60, 0, -1)  # most-recent-first
        ]
        result = draw_rating_history_chart("TestUser", history)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size == (700, 400)

    def test_partial_openskill_data(self):
        """Test chart where only some entries have OpenSkill data."""
        history = [
            _make_history_entry(rating=1600, won=True, os_mu=40),
            _make_history_entry(rating=1550, won=False),  # No OS
            _make_history_entry(rating=1500, won=True, os_mu=35),
        ]
        result = draw_rating_history_chart("TestUser", history)
        img = Image.open(result)
        assert img.format == "PNG"

    def test_flat_ratings(self):
        """Test chart with identical ratings (div-by-zero guard)."""
        history = [
            _make_history_entry(rating=1500, won=True, os_mu=35),
            _make_history_entry(rating=1500, won=False, os_mu=35),
            _make_history_entry(rating=1500, won=True, os_mu=35),
        ]
        result = draw_rating_history_chart("TestUser", history)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size == (700, 400)

    def test_won_none_renders_grey(self):
        """Test that won=None entries render without error (grey dot)."""
        history = [
            _make_history_entry(rating=1600, won=True),
            _make_history_entry(rating=1550, won=None),
            _make_history_entry(rating=1500, won=False),
        ]
        result = draw_rating_history_chart("TestUser", history)
        img = Image.open(result)
        assert img.format == "PNG"

    def test_returns_seekable_bytesio(self):
        """Test that returned BytesIO is seeked to start."""
        history = [
            _make_history_entry(rating=1600, won=True),
            _make_history_entry(rating=1500, won=False),
        ]
        result = draw_rating_history_chart("TestUser", history)
        assert isinstance(result, BytesIO)
        assert result.tell() == 0
        # Should be readable without seeking
        data = result.read(8)
        assert data[:4] == b"\x89PNG"
