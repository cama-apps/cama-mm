"""Tests for Discord embed safety utilities."""

import discord

from utils.embed_safety import (
    EMBED_LIMITS,
    add_lines_field,
    truncate_field,
    validate_embed,
)


class TestTruncateField:
    """Tests for truncate_field function."""

    def test_short_text_unchanged(self):
        """Text under limit should be returned unchanged."""
        text = "Short text"
        result = truncate_field(text)
        assert result == text

    def test_exact_limit_unchanged(self):
        """Text exactly at limit should be returned unchanged."""
        text = "x" * 1024
        result = truncate_field(text)
        assert result == text
        assert len(result) == 1024

    def test_over_limit_truncated(self):
        """Text over limit should be truncated with ellipsis."""
        text = "x" * 1100
        result = truncate_field(text)
        assert len(result) == 1024
        assert result.endswith("...")

    def test_custom_limit(self):
        """Custom limit should be respected."""
        text = "x" * 500
        result = truncate_field(text, max_len=100)
        assert len(result) == 100
        assert result.endswith("...")

    def test_empty_text(self):
        """Empty text should be returned unchanged."""
        result = truncate_field("")
        assert result == ""

    def test_truncation_preserves_content(self):
        """Truncation should preserve content before ellipsis."""
        text = "Hello World" + "x" * 1100
        result = truncate_field(text)
        assert result.startswith("Hello World")


class MockField:
    """Mock Discord embed field."""

    def __init__(self, name: str, value: str):
        self.name = name
        self.value = value


class MockFooter:
    """Mock Discord embed footer."""

    def __init__(self, text: str):
        self.text = text


class MockEmbed:
    """Mock Discord embed for testing."""

    def __init__(self):
        self.title = None
        self.description = None
        self.fields = []
        self.footer = None

    def add_field(self, name: str, value: str):
        self.fields.append(MockField(name, value))


class TestValidateEmbed:
    """Tests for validate_embed function."""

    def test_valid_embed_no_errors(self):
        """Valid embed should return no errors."""
        embed = MockEmbed()
        embed.title = "Short title"
        embed.description = "Short description"
        embed.add_field("Field", "Value")
        errors = validate_embed(embed)
        assert errors == []

    def test_title_too_long(self):
        """Over-long title should return error."""
        embed = MockEmbed()
        embed.title = "x" * (EMBED_LIMITS["title"] + 1)
        errors = validate_embed(embed)
        assert len(errors) == 1
        assert "Title" in errors[0]

    def test_description_too_long(self):
        """Over-long description should return error."""
        embed = MockEmbed()
        embed.description = "x" * (EMBED_LIMITS["description"] + 1)
        errors = validate_embed(embed)
        assert len(errors) == 1
        assert "Description" in errors[0]

    def test_field_value_too_long(self):
        """Over-long field value should return error."""
        embed = MockEmbed()
        embed.add_field("Test Field", "x" * (EMBED_LIMITS["field_value"] + 1))
        errors = validate_embed(embed)
        assert len(errors) == 1
        assert "Field 0" in errors[0]
        assert "Test Field" in errors[0]

    def test_field_name_too_long(self):
        """Over-long field name should return error."""
        embed = MockEmbed()
        embed.add_field("x" * (EMBED_LIMITS["field_name"] + 1), "Value")
        errors = validate_embed(embed)
        assert len(errors) == 1
        assert "name" in errors[0]

    def test_footer_too_long(self):
        """Over-long footer should return error."""
        embed = MockEmbed()
        embed.footer = MockFooter("x" * (EMBED_LIMITS["footer"] + 1))
        errors = validate_embed(embed)
        assert len(errors) == 1
        assert "Footer" in errors[0]

    def test_too_many_fields(self):
        """Too many fields should return error."""
        embed = MockEmbed()
        for i in range(EMBED_LIMITS["max_fields"] + 1):
            embed.add_field(f"Field {i}", "Value")
        errors = validate_embed(embed)
        assert len(errors) == 1
        assert "Too many fields" in errors[0]

    def test_multiple_errors(self):
        """Multiple violations should return multiple errors."""
        embed = MockEmbed()
        embed.description = "x" * (EMBED_LIMITS["description"] + 1)
        embed.add_field("Test", "x" * (EMBED_LIMITS["field_value"] + 1))
        errors = validate_embed(embed)
        assert len(errors) == 2


class TestAddLinesField:
    """Tests for add_lines_field — packs lines into one or more valid fields."""

    def test_empty_lines_adds_no_field(self):
        """Empty input is a no-op: no field is added."""
        embed = discord.Embed()
        add_lines_field(embed, "Empty", [])
        assert len(embed.fields) == 0

    def test_short_list_single_field(self):
        """A list that fits stays a single field with the given name."""
        embed = discord.Embed()
        add_lines_field(embed, "Greek", ["alpha", "beta", "gamma"])
        assert len(embed.fields) == 1
        assert embed.fields[0].name == "Greek"
        assert embed.fields[0].value == "alpha\nbeta\ngamma"

    def test_long_list_splits_without_dropping_lines(self):
        """A list whose joined text exceeds the limit splits across fields, with
        every line preserved in order — the whole point is to never hide items."""
        lines = [f"item-{i:03d}-" + "x" * 50 for i in range(40)]  # ~2.3k chars
        assert len("\n".join(lines)) > EMBED_LIMITS["field_value"]

        embed = discord.Embed()
        add_lines_field(embed, "Stuff", lines)

        assert len(embed.fields) >= 2  # had to split
        for field in embed.fields:
            assert len(field.value) <= EMBED_LIMITS["field_value"]
        # First field keeps the name; continuations use a zero-width blank.
        assert embed.fields[0].name == "Stuff"
        for field in embed.fields[1:]:
            assert len(field.name) == 1 and ord(field.name) == 0x200B
        # No line lost or reordered across the split.
        recovered = [ln for field in embed.fields for ln in field.value.split("\n")]
        assert recovered == lines

    def test_single_overlong_line_is_truncated(self):
        """A single line longer than the limit can't be split, so it's truncated
        rather than emitted as an invalid (over-1024) field."""
        embed = discord.Embed()
        add_lines_field(embed, "Big", ["y" * 2000])
        assert len(embed.fields) == 1
        assert len(embed.fields[0].value) <= EMBED_LIMITS["field_value"]
        assert embed.fields[0].value.endswith("...")
