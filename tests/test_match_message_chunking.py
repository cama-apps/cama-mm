from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

from commands.match import (
    DISCORD_MESSAGE_MAX_CHARS,
    MatchCommands,
    _chunk_discord_content,
)


def test_chunk_discord_content_prefers_newlines() -> None:
    first_line = "a" * 1500
    second_line = "b" * 600

    chunks = _chunk_discord_content(f"{first_line}\n{second_line}")

    assert chunks == [first_line, second_line]
    assert all(len(chunk) <= DISCORD_MESSAGE_MAX_CHARS for chunk in chunks)


def test_chunk_discord_content_hard_splits_oversized_line() -> None:
    content = "x" * (DISCORD_MESSAGE_MAX_CHARS * 2 + 1)

    chunks = _chunk_discord_content(content)

    assert [len(chunk) for chunk in chunks] == [2000, 2000, 1]
    assert "".join(chunks) == content


@pytest.mark.asyncio
async def test_record_announcement_chunks_the_reliable_result_message() -> None:
    cog = MatchCommands(Mock(), Mock(), Mock(), Mock())
    interaction = SimpleNamespace(followup=AsyncMock())
    result_message = "✅ Match recorded.\n" + "\n".join(
        f"payout-{index}: {'x' * 90}" for index in range(45)
    )
    await cog._send_record_announcement(interaction, result_message)

    sent = [call.args[0] for call in interaction.followup.send.call_args_list]
    result_chunks = _chunk_discord_content(result_message)
    assert sent == result_chunks
    assert len(result_chunks) > 1
    assert all(len(content) <= DISCORD_MESSAGE_MAX_CHARS for content in sent)
    assert all(call.kwargs == {"ephemeral": False} for call in interaction.followup.send.call_args_list)
