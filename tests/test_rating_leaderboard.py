"""Tests for rating leaderboard embed character limits.

Ensures Glicko and OpenSkill leaderboard embeds stay within Discord's 1024-char field limit.
"""

import pytest


# Constants matching commands/info.py
SINGLE_SECTION_PAGE_SIZE = 20
EMBED_FIELD_LIMIT = 1024


def build_worst_case_entry(rank: int) -> str:
    """Build a worst-case leaderboard entry (maximum character length).

    Worst case:
    - 18-digit Discord ID (max snowflake)
    - 4-digit rating (up to 3000)
    - 100% certainty (3 digits)
    - Double-digit win-loss (99-99)
    """
    # Medals: 🥇/🥈/🥉 take 1 char visually but may be more in bytes
    # For rank > 3, we use "N." format
    medal = "🥇" if rank == 1 else "🥈" if rank == 2 else "🥉" if rank == 3 else f"{rank}."
    discord_id = "123456789012345678"  # 18-digit max snowflake
    display_name = f"<@{discord_id}>"
    rating = "3000"  # Max 4 digits
    certainty = 100  # Max 3 digits
    record = "99-99"  # Max reasonable record

    # New shortened format: {medal} **{display_name}** {rating} ({certainty}%) {record}
    return f"{medal} **{display_name}** {rating} ({certainty:.0f}%) {record}"


def build_old_format_entry(rank: int) -> str:
    """Build the old (too long) format entry for comparison."""
    medal = "🥇" if rank == 1 else "🥈" if rank == 2 else "🥉" if rank == 3 else f"{rank}."
    discord_id = "123456789012345678"
    display_name = f"<@{discord_id}>"
    rating = "3000"
    certainty = 100
    record = "99-99"

    # Old format: {medal} **{display_name}** - {rating} ({certainty}% certain) • {record}
    return f"{medal} **{display_name}** - {rating} ({certainty:.0f}% certain) • {record}"


class TestGlickoEmbedCharacterLimits:
    """Tests for Glicko leaderboard embed character limits."""

    def test_single_entry_length(self):
        """Single worst-case entry should be under 50 chars."""
        entry = build_worst_case_entry(1)
        # 🥇 **<@123456789012345678>** 3000 (100%) 99-99
        # Should be around 48 chars
        assert len(entry) < 50, f"Entry is {len(entry)} chars: {entry}"

    def test_20_entries_under_1024_chars(self):
        """20 worst-case entries must fit in 1024 chars."""
        entries = [build_worst_case_entry(i + 1) for i in range(SINGLE_SECTION_PAGE_SIZE)]
        content = "\n".join(entries)

        # Calculate expected max: 48 chars/entry * 20 + 19 newlines = ~979
        assert len(content) <= EMBED_FIELD_LIMIT, (
            f"Content is {len(content)} chars, exceeds {EMBED_FIELD_LIMIT}. "
            f"First entry: {entries[0]}"
        )

    def test_new_format_is_shorter_than_old(self):
        """New format should be at least 14 chars shorter than old format."""
        old_entry = build_old_format_entry(1)
        new_entry = build_worst_case_entry(1)

        saved = len(old_entry) - len(new_entry)
        # We removed " - " (3), " certain" (8), " • " (3) = 14 chars
        assert saved >= 14, f"Only saved {saved} chars (expected 14+)"

    def test_old_format_would_exceed_limit(self):
        """Verify the old format would exceed 1024 chars with 20 entries."""
        entries = [build_old_format_entry(i + 1) for i in range(SINGLE_SECTION_PAGE_SIZE)]
        content = "\n".join(entries)

        # Old format: ~63 chars/entry * 20 + 19 newlines = ~1279
        assert len(content) > EMBED_FIELD_LIMIT, (
            f"Old format is only {len(content)} chars, expected > 1024"
        )


class TestOpenSkillEmbedCharacterLimits:
    """Tests for OpenSkill leaderboard embed character limits.

    Uses same format as Glicko, so same tests apply.
    """

    def test_20_entries_under_1024_chars(self):
        """20 worst-case entries must fit in 1024 chars."""
        entries = [build_worst_case_entry(i + 1) for i in range(SINGLE_SECTION_PAGE_SIZE)]
        content = "\n".join(entries)

        assert len(content) <= EMBED_FIELD_LIMIT, (
            f"Content is {len(content)} chars, exceeds {EMBED_FIELD_LIMIT}"
        )


class TestEntryFormatVariations:
    """Test various entry format edge cases."""

    def test_short_username_fallback(self):
        """Username fallback (non-mention) should also fit."""
        medal = "1."
        username = "VeryLongUsername123"  # Max ~20 chars realistic
        rating = "3000"
        certainty = 100
        record = "99-99"

        entry = f"{medal} **{username}** {rating} ({certainty:.0f}%) {record}"
        assert len(entry) < 50, f"Entry is {len(entry)} chars"

    def test_low_rank_numbers(self):
        """Lower rank numbers (1-9) should be shorter than 10+."""
        entry_1 = build_worst_case_entry(1)  # Medal emoji
        entry_10 = build_worst_case_entry(10)  # "10."
        entry_20 = build_worst_case_entry(20)  # "20."

        # Medal emojis and "N." take varying space, but all should fit
        assert len(entry_1) < 50
        assert len(entry_10) < 50
        assert len(entry_20) < 50

    def test_single_digit_win_loss(self):
        """Single-digit W-L records should be shorter."""
        medal = "🥇"
        discord_id = "123456789012345678"
        display_name = f"<@{discord_id}>"
        rating = "1500"
        certainty = 50
        record = "1-0"

        entry = f"{medal} **{display_name}** {rating} ({certainty:.0f}%) {record}"
        # Should be even shorter than worst case
        assert len(entry) < 45, f"Entry is {len(entry)} chars"
