"""
Tests for drawing utilities.
"""

from io import BytesIO

from PIL import Image

from utils.drawing import (
    draw_attribute_distribution,
    draw_lane_distribution,
    draw_matches_table,
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
