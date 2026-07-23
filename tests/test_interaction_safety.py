"""Tests for utils/interaction_safety.py.

The key regressions guarded here: a deferred interaction whose followup send
fails must never be left on a silent "thinking…" spinner (it falls back to a
channel send), and ephemeral content must never leak into the public channel.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import discord
import pytest

from utils.interaction_safety import (
    safe_defer,
    safe_followup,
    send_public_or_ephemeral,
    update_lobby_message_closed,
)


def _http_error(status: int = 403, reason: str = "Forbidden", message: str = "Missing Permissions"):
    """Build a real discord.HTTPException without a live aiohttp response."""
    return discord.HTTPException(SimpleNamespace(status=status, reason=reason), message)


class _Recorder:
    """Records send() kwargs; optionally raises a configured exception."""

    def __init__(self, *, raises: Exception | None = None, returns="ok"):
        self.calls: list[dict] = []
        self._raises = raises
        self._returns = returns

    async def send(self, **kwargs):
        self.calls.append(kwargs)
        if self._raises is not None:
            raise self._raises
        return self._returns


class _StubInteraction:
    def __init__(self, *, followup=None, channel=None, is_done: bool = False):
        self.id = 123
        self.followup = followup or _Recorder()
        self.channel = channel
        self.response = SimpleNamespace(is_done=lambda: is_done)


@pytest.mark.asyncio
async def test_safe_defer_forwards_thinking_flag():
    calls = []

    class _Response:
        async def defer(self, **kwargs):
            calls.append(kwargs)

    interaction = SimpleNamespace(response=_Response())

    result = await safe_defer(interaction, thinking=True)

    assert result is True
    assert calls == [{"ephemeral": False, "thinking": True}]


@pytest.mark.asyncio
async def test_safe_followup_happy_path_sends_followup():
    interaction = _StubInteraction()

    result = await safe_followup(interaction, content="hi", ephemeral=True)

    assert result == "ok"
    assert interaction.followup.calls[-1]["content"] == "hi"
    assert interaction.followup.calls[-1]["ephemeral"] is True


@pytest.mark.asyncio
async def test_followup_failure_after_defer_falls_back_to_channel():
    """Regression: a deferred (is_done=True) interaction whose followup send fails
    must fall back to a channel send, NOT be silently swallowed as a 'duplicate
    handler' — that swallow is what left /dig shop stuck on 'thinking…'."""
    failing_followup = _Recorder(raises=_http_error())
    channel = _Recorder(returns="channel-msg")
    interaction = _StubInteraction(followup=failing_followup, channel=channel, is_done=True)

    result = await safe_followup(interaction, content="shop", embed=None)

    assert result == "channel-msg"
    assert len(channel.calls) == 1
    assert channel.calls[0]["content"] == "shop"


@pytest.mark.asyncio
async def test_ephemeral_followup_failure_does_not_leak_to_channel():
    """An ephemeral message must never spill into the public channel: on failure
    it re-raises rather than falling back to a public channel send."""
    failing_followup = _Recorder(raises=_http_error())
    channel = _Recorder()
    interaction = _StubInteraction(followup=failing_followup, channel=channel, is_done=True)

    with pytest.raises(discord.HTTPException):
        await safe_followup(interaction, content="secret", ephemeral=True)

    assert channel.calls == []


@pytest.mark.asyncio
async def test_send_public_or_ephemeral_retries_ephemerally_without_attachment():
    """When the public send fails (and there's no channel to fall back to), the
    helper retries privately and drops the decorative attachment — guaranteeing
    the user still sees their result. This is the /dig shop guarantee."""

    class _FailPublicOnly:
        def __init__(self):
            self.calls: list[dict] = []

        async def send(self, **kwargs):
            self.calls.append(kwargs)
            if not kwargs.get("ephemeral"):
                raise _http_error()
            return "ephemeral-ok"

    followup = _FailPublicOnly()
    interaction = _StubInteraction(followup=followup, channel=None, is_done=True)
    sentinel_file = object()

    result = await send_public_or_ephemeral(interaction, embed="E", file=sentinel_file)

    assert result == "ephemeral-ok"
    # The first (public) attempt carried the attachment and failed.
    assert followup.calls[0].get("file") is sentinel_file
    # The successful ephemeral retry must NOT carry the attachment.
    ephemeral_calls = [c for c in followup.calls if c.get("ephemeral")]
    assert ephemeral_calls
    assert "file" not in ephemeral_calls[-1]


@pytest.mark.asyncio
async def test_send_public_or_ephemeral_last_resort_names_the_error():
    """If even the ephemeral embed retry fails, the user gets an ephemeral note
    that names the exception (self-diagnosing, since prod logs aren't available)."""

    class _FailUntilPlainText:
        def __init__(self):
            self.calls: list[dict] = []

        async def send(self, **kwargs):
            self.calls.append(kwargs)
            # Anything carrying an embed fails; only the plain-text note succeeds.
            if kwargs.get("embed") is not None:
                raise _http_error(message="Cannot send embeds here")
            return "note-ok"

    followup = _FailUntilPlainText()
    interaction = _StubInteraction(followup=followup, channel=None, is_done=True)

    result = await send_public_or_ephemeral(interaction, embed="E", file=object())

    assert result == "note-ok"
    note = followup.calls[-1]
    assert note["ephemeral"] is True
    assert "HTTPException" in note["content"]


@pytest.mark.asyncio
async def test_non_ephemeral_failure_with_no_channel_reraises():
    """If the followup fails and there is no channel to fall back to, the error
    must propagate (so the global handler can log/notify) — never vanish."""
    failing_followup = _Recorder(raises=_http_error())
    interaction = _StubInteraction(followup=failing_followup, channel=None, is_done=True)

    with pytest.raises(discord.HTTPException):
        await safe_followup(interaction, content="x")


class _StubFile:
    def __init__(self):
        self.reset_called = False

    def reset(self, *, seek: bool = True):
        self.reset_called = True


@pytest.mark.asyncio
async def test_channel_fallback_rewinds_and_forwards_attachment():
    """On the channel fallback, a partially-consumed attachment is rewound and
    forwarded, so the user gets the file rather than a zero-byte send."""
    failing_followup = _Recorder(raises=_http_error())
    channel = _Recorder(returns="channel-msg")
    interaction = _StubInteraction(followup=failing_followup, channel=channel, is_done=True)
    attachment = _StubFile()

    result = await safe_followup(interaction, embed="E", file=attachment)

    assert result == "channel-msg"
    assert attachment.reset_called is True
    assert channel.calls[0]["file"] is attachment


@pytest.mark.asyncio
async def test_close_lobby_edits_partial_message_without_fetch():
    """A persisted lobby message ID needs only the closing PATCH request."""
    message = SimpleNamespace(edit=AsyncMock())
    channel = SimpleNamespace(
        fetch_message=AsyncMock(),
        get_partial_message=MagicMock(return_value=message),
    )
    bot = SimpleNamespace(
        get_channel=MagicMock(return_value=channel),
        fetch_channel=AsyncMock(),
    )
    lobby_service = MagicMock()
    lobby_service.get_lobby_message_id.return_value = 123
    lobby_service.get_lobby_channel_id.return_value = 456

    await update_lobby_message_closed(
        bot,
        lobby_service,
        reason="Lobby Reset",
        guild_id=42,
    )

    channel.fetch_message.assert_not_awaited()
    channel.get_partial_message.assert_called_once_with(123)
    message.edit.assert_awaited_once()
    assert message.edit.await_args.kwargs["embed"].title == "🚫 Lobby Reset"
    assert message.edit.await_args.kwargs["view"] is None
