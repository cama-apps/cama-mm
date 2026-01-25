"""Tests for utils/command_helpers.py - Discord command helper utilities."""

from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

from services.result import Result
from utils.command_helpers import (
    format_result_error,
    handle_result,
    handle_result_with_embed,
)


@pytest.fixture
def mock_interaction():
    """Create a mock Discord interaction."""
    interaction = MagicMock(spec=discord.Interaction)
    interaction.followup = MagicMock()
    interaction.followup.send = AsyncMock()
    return interaction


class TestHandleResult:
    """Tests for handle_result function."""

    @pytest.mark.asyncio
    async def test_success_without_message(self, mock_interaction):
        """Successful result with no message should return True without sending."""
        result = Result.ok({"key": "value"})

        with patch("utils.command_helpers.safe_followup", new_callable=AsyncMock) as mock_followup:
            success = await handle_result(mock_interaction, result)

            assert success is True
            mock_followup.assert_not_called()

    @pytest.mark.asyncio
    async def test_success_with_message(self, mock_interaction):
        """Successful result with message should send the message."""
        result = Result.ok({"key": "value"})

        with patch("utils.command_helpers.safe_followup", new_callable=AsyncMock) as mock_followup:
            success = await handle_result(
                mock_interaction, result, success_msg="Operation completed!"
            )

            assert success is True
            mock_followup.assert_awaited_once_with(
                mock_interaction, content="Operation completed!", ephemeral=True
            )

    @pytest.mark.asyncio
    async def test_failure_sends_error(self, mock_interaction):
        """Failed result should send error message and return False."""
        result = Result.fail("Something went wrong")

        with patch("utils.command_helpers.safe_followup", new_callable=AsyncMock) as mock_followup:
            success = await handle_result(mock_interaction, result)

            assert success is False
            mock_followup.assert_awaited_once()
            call_kwargs = mock_followup.call_args.kwargs
            assert "Something went wrong" in call_kwargs["content"]
            assert call_kwargs["ephemeral"] is True

    @pytest.mark.asyncio
    async def test_failure_with_error_code(self, mock_interaction):
        """Failed result with error code should include code in message."""
        result = Result.fail("Insufficient funds", code="BALANCE_ERROR")

        with patch("utils.command_helpers.safe_followup", new_callable=AsyncMock) as mock_followup:
            success = await handle_result(mock_interaction, result)

            assert success is False
            call_kwargs = mock_followup.call_args.kwargs
            assert "BALANCE_ERROR" in call_kwargs["content"]
            assert "Insufficient funds" in call_kwargs["content"]

    @pytest.mark.asyncio
    async def test_ephemeral_parameter(self, mock_interaction):
        """Ephemeral parameter should be passed through."""
        result = Result.ok(None)

        with patch("utils.command_helpers.safe_followup", new_callable=AsyncMock) as mock_followup:
            await handle_result(
                mock_interaction, result, success_msg="Done", ephemeral=False
            )

            call_kwargs = mock_followup.call_args.kwargs
            assert call_kwargs["ephemeral"] is False


class TestHandleResultWithEmbed:
    """Tests for handle_result_with_embed function."""

    @pytest.mark.asyncio
    async def test_success_without_embed(self, mock_interaction):
        """Successful result with no embed should return True without sending."""
        result = Result.ok({"key": "value"})

        with patch("utils.command_helpers.safe_followup", new_callable=AsyncMock) as mock_followup:
            success = await handle_result_with_embed(mock_interaction, result)

            assert success is True
            mock_followup.assert_not_called()

    @pytest.mark.asyncio
    async def test_success_with_embed(self, mock_interaction):
        """Successful result with embed should send the embed."""
        result = Result.ok({"key": "value"})
        embed = discord.Embed(title="Success", description="It worked!")

        with patch("utils.command_helpers.safe_followup", new_callable=AsyncMock) as mock_followup:
            success = await handle_result_with_embed(
                mock_interaction, result, success_embed=embed
            )

            assert success is True
            mock_followup.assert_awaited_once_with(
                mock_interaction, embed=embed, ephemeral=False
            )

    @pytest.mark.asyncio
    async def test_failure_sends_error(self, mock_interaction):
        """Failed result should send error message and return False."""
        result = Result.fail("Something went wrong")

        with patch("utils.command_helpers.safe_followup", new_callable=AsyncMock) as mock_followup:
            success = await handle_result_with_embed(mock_interaction, result)

            assert success is False
            call_kwargs = mock_followup.call_args.kwargs
            assert "Something went wrong" in call_kwargs["content"]
            assert call_kwargs["ephemeral"] is True


class TestFormatResultError:
    """Tests for format_result_error function."""

    def test_success_returns_empty(self):
        """Successful result should return empty string."""
        result = Result.ok({"key": "value"})
        assert format_result_error(result) == ""

    def test_failure_returns_error(self):
        """Failed result should return the error message."""
        result = Result.fail("Something went wrong")
        assert format_result_error(result) == "Something went wrong"

    def test_failure_with_code_includes_code(self):
        """Failed result with code should include the code."""
        result = Result.fail("Insufficient funds", code="BALANCE_ERROR")
        formatted = format_result_error(result)
        assert "[BALANCE_ERROR]" in formatted
        assert "Insufficient funds" in formatted

    def test_failure_without_error_message(self):
        """Failed result without error message should return 'Unknown error'."""
        result = Result(success=False, error=None)
        assert format_result_error(result) == "Unknown error"
