"""Tests for Discord embed safety utilities."""

import pytest

from utils.embed_safety import truncate_field, validate_embed, EMBED_LIMITS


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
