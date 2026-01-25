"""Tests for utils/pin_helpers.py - safe unpin all bot messages."""

import pytest
from unittest.mock import AsyncMock, MagicMock, patch
import discord


class TestSafeUnpinAllBotMessages:
    """Tests for safe_unpin_all_bot_messages utility."""

    @pytest.fixture
    def bot_user(self):
        """Mock bot user with a specific ID."""
        user = MagicMock(spec=discord.User)
        user.id = 12345
        return user

    @pytest.fixture
    def make_pinned_message(self):
        """Factory for creating mock pinned messages."""
        def _make(author_id: int, msg_id: int):
            msg = AsyncMock(spec=discord.Message)
            msg.id = msg_id
            msg.author = MagicMock()
            msg.author.id = author_id
            msg.unpin = AsyncMock()
            return msg
        return _make

    @pytest.mark.asyncio
    async def test_unpins_all_bot_messages(self, bot_user, make_pinned_message):
        """Should unpin ALL pinned messages authored by the bot."""
        from utils.pin_helpers import safe_unpin_all_bot_messages

        # Create multiple bot messages and one user message
        bot_msg1 = make_pinned_message(bot_user.id, 100)
        bot_msg2 = make_pinned_message(bot_user.id, 101)
        user_msg = make_pinned_message(99999, 200)  # Different author

        channel = AsyncMock()
        channel.pins = AsyncMock(return_value=[bot_msg1, bot_msg2, user_msg])

        unpinned_count = await safe_unpin_all_bot_messages(channel, bot_user)

        assert unpinned_count == 2
        bot_msg1.unpin.assert_called_once()
        bot_msg2.unpin.assert_called_once()
        user_msg.unpin.assert_not_called()

    @pytest.mark.asyncio
    async def test_returns_zero_for_no_bot_messages(self, bot_user, make_pinned_message):
        """Should return 0 when no bot messages are pinned."""
        from utils.pin_helpers import safe_unpin_all_bot_messages

        user_msg = make_pinned_message(99999, 200)

        channel = AsyncMock()
        channel.pins = AsyncMock(return_value=[user_msg])

        unpinned_count = await safe_unpin_all_bot_messages(channel, bot_user)

        assert unpinned_count == 0
        user_msg.unpin.assert_not_called()

    @pytest.mark.asyncio
    async def test_returns_zero_for_no_channel(self, bot_user):
        """Should return 0 when channel is None."""
        from utils.pin_helpers import safe_unpin_all_bot_messages

        unpinned_count = await safe_unpin_all_bot_messages(None, bot_user)

        assert unpinned_count == 0

    @pytest.mark.asyncio
    async def test_returns_zero_for_channel_without_pins(self, bot_user):
        """Should return 0 when channel has no pins method."""
        from utils.pin_helpers import safe_unpin_all_bot_messages

        # Channel without pins attribute
        channel = MagicMock()
        del channel.pins

        unpinned_count = await safe_unpin_all_bot_messages(channel, bot_user)

        assert unpinned_count == 0

    @pytest.mark.asyncio
    async def test_handles_fetch_pins_error_gracefully(self, bot_user):
        """Should return 0 and not raise when pins() fails."""
        from utils.pin_helpers import safe_unpin_all_bot_messages

        channel = AsyncMock()
        channel.pins = AsyncMock(side_effect=discord.HTTPException(MagicMock(), "Error"))

        unpinned_count = await safe_unpin_all_bot_messages(channel, bot_user)

        assert unpinned_count == 0

    @pytest.mark.asyncio
    async def test_handles_forbidden_on_unpin(self, bot_user, make_pinned_message):
        """Should continue unpinning other messages when one fails with Forbidden."""
        from utils.pin_helpers import safe_unpin_all_bot_messages

        bot_msg1 = make_pinned_message(bot_user.id, 100)
        bot_msg1.unpin = AsyncMock(side_effect=discord.Forbidden(MagicMock(), "No permission"))
        bot_msg2 = make_pinned_message(bot_user.id, 101)

        channel = AsyncMock()
        channel.pins = AsyncMock(return_value=[bot_msg1, bot_msg2])

        # Should not raise, and should continue to msg2
        unpinned_count = await safe_unpin_all_bot_messages(channel, bot_user)

        # Only 1 was successfully unpinned (msg2), msg1 failed
        assert unpinned_count == 1
        bot_msg1.unpin.assert_called_once()
        bot_msg2.unpin.assert_called_once()

    @pytest.mark.asyncio
    async def test_handles_generic_exception_on_unpin(self, bot_user, make_pinned_message):
        """Should continue unpinning other messages when one fails with generic error."""
        from utils.pin_helpers import safe_unpin_all_bot_messages

        bot_msg1 = make_pinned_message(bot_user.id, 100)
        bot_msg1.unpin = AsyncMock(side_effect=Exception("Random error"))
        bot_msg2 = make_pinned_message(bot_user.id, 101)

        channel = AsyncMock()
        channel.pins = AsyncMock(return_value=[bot_msg1, bot_msg2])

        unpinned_count = await safe_unpin_all_bot_messages(channel, bot_user)

        # Only msg2 succeeded
        assert unpinned_count == 1
        bot_msg1.unpin.assert_called_once()
        bot_msg2.unpin.assert_called_once()

    @pytest.mark.asyncio
    async def test_empty_pins_list(self, bot_user):
        """Should return 0 when pins list is empty."""
        from utils.pin_helpers import safe_unpin_all_bot_messages

        channel = AsyncMock()
        channel.pins = AsyncMock(return_value=[])

        unpinned_count = await safe_unpin_all_bot_messages(channel, bot_user)

        assert unpinned_count == 0
