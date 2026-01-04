"""
Tests for embed utilities.
"""

from datetime import datetime
from unittest.mock import MagicMock

from utils.embeds import create_lobby_embed


class TestLobbyEmbedTimestamp:
    """Test that lobby embed uses Discord dynamic timestamps."""

    def test_lobby_embed_uses_discord_timestamp_format(self):
        """Verify lobby footer uses <t:TIMESTAMP:t> format for user-local time."""
        # Create a lobby with a known created_at time
        lobby = MagicMock()
        lobby.created_at = datetime(2026, 1, 2, 12, 30, 0)
        lobby.get_player_count.return_value = 5

        embed = create_lobby_embed(lobby, players=[], player_ids={}, ready_threshold=10)

        footer_text = embed.footer.text
        expected_ts = int(lobby.created_at.timestamp())

        assert f"<t:{expected_ts}:t>" in footer_text
        assert "Opened at" in footer_text

    def test_lobby_embed_no_created_at_fallback(self):
        """Verify fallback when lobby has no created_at."""
        lobby = MagicMock()
        lobby.created_at = None
        lobby.get_player_count.return_value = 0

        embed = create_lobby_embed(lobby, players=[], player_ids={}, ready_threshold=10)

        assert embed.footer.text == "Opened just now"

    def test_lobby_embed_timestamp_is_unix_epoch(self):
        """Verify the timestamp is a valid Unix epoch integer."""
        lobby = MagicMock()
        lobby.created_at = datetime.now()
        lobby.get_player_count.return_value = 3

        embed = create_lobby_embed(lobby, players=[], player_ids={}, ready_threshold=10)

        footer_text = embed.footer.text
        # Extract timestamp from <t:TIMESTAMP:t>
        import re

        match = re.search(r"<t:(\d+):t>", footer_text)
        assert match is not None, f"No Discord timestamp found in: {footer_text}"

        timestamp = int(match.group(1))
        # Verify it's a reasonable Unix timestamp (after year 2020)
        assert timestamp > 1577836800, "Timestamp should be after 2020"
