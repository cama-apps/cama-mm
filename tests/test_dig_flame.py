"""Atmospheric flame-post helper: catastrophic line pool plus the
fire-and-forget channel-post primitive and its failure-tolerance contract."""

import asyncio
import random

import pytest

from services.dig_flame import (
    CATASTROPHIC_LINES,
    CATASTROPHIC_PREFIX,
    pick_catastrophic_line,
    post_atmospheric,
    post_catastrophic,
)


class _RecordingChannel:
    """A channel stand-in whose ``send`` coroutine records its argument.

    Mirrors how a real discord.py channel exposes an async ``send``; the
    flame helper must spawn the coroutine on the running loop and pass the
    fully-formatted line through unmodified.
    """

    def __init__(self):
        self.sent: list[str] = []

    async def send(self, content):
        self.sent.append(content)


class TestCatastrophicLinePool:
    """The catastrophic line pool feeds player-visible cave-in flavor."""

    def test_pool_is_non_empty(self):
        # An empty pool would make random.choice raise inside
        # pick_catastrophic_line, breaking every cave-in post.
        assert len(CATASTROPHIC_LINES) > 0

    def test_lines_are_non_blank_strings(self):
        # post_atmospheric drops falsy lines silently, so a blank entry
        # would be a permanently invisible flame line.
        for line in CATASTROPHIC_LINES:
            assert isinstance(line, str)
            assert line.strip()

    def test_lines_are_unique(self):
        # Duplicates skew the random distribution toward the repeated line.
        assert len(set(CATASTROPHIC_LINES)) == len(CATASTROPHIC_LINES)

    def test_lines_contain_no_proper_nouns_marker(self):
        # The module contract is "no mechanics exposition" -- lines should
        # never leak internal terms players would parse as mechanics.
        banned = ("luminosity", "prestige", "JC", "jopacoin", "cave-in chance")
        for line in CATASTROPHIC_LINES:
            lowered = line.lower()
            for term in banned:
                assert term.lower() not in lowered, f"{term!r} leaked in {line!r}"

    def test_pick_returns_member_of_pool(self):
        # The picker must only ever yield canonical lines.
        random.seed(0)
        for _ in range(100):
            assert pick_catastrophic_line() in CATASTROPHIC_LINES

    def test_pick_is_seeded_deterministic(self):
        # Seeded RNG must reproduce -- callers rely on this for testability.
        random.seed(1234)
        first = pick_catastrophic_line()
        random.seed(1234)
        assert pick_catastrophic_line() == first

    def test_pick_covers_multiple_lines(self):
        # A picker stuck on one line would make cave-ins feel repetitive;
        # over many draws we should see real spread across the pool.
        random.seed(99)
        seen = {pick_catastrophic_line() for _ in range(400)}
        assert len(seen) > 1


class TestPostAtmospheric:
    """post_atmospheric formats and fire-and-forgets a single channel post."""

    @pytest.mark.asyncio
    async def test_formats_with_prefix_and_italics(self):
        # The visible contract: "<prefix> *<line>*" -- prefix outside the
        # italics, line trimmed and wrapped in asterisks.
        channel = _RecordingChannel()
        post_atmospheric(channel, "X", "a tunnel folds shut")
        await asyncio.sleep(0)
        assert channel.sent == ["X *a tunnel folds shut*"]

    @pytest.mark.asyncio
    async def test_strips_surrounding_whitespace_from_line(self):
        # Lines may arrive padded; the helper trims before italicizing so
        # the asterisks hug the text.
        channel = _RecordingChannel()
        post_atmospheric(channel, "P", "   spaced line   ")
        await asyncio.sleep(0)
        assert channel.sent == ["P *spaced line*"]

    @pytest.mark.asyncio
    async def test_none_channel_is_a_noop(self):
        # Callers pass interaction.channel, which can be None in DMs; the
        # helper must tolerate that without raising.
        post_atmospheric(None, "P", "line")
        await asyncio.sleep(0)  # nothing to await, just proves no crash

    @pytest.mark.asyncio
    async def test_empty_line_is_a_noop(self):
        # A falsy line must not produce a bare "<prefix> **" post.
        channel = _RecordingChannel()
        post_atmospheric(channel, "P", "")
        await asyncio.sleep(0)
        assert channel.sent == []

    @pytest.mark.asyncio
    async def test_channel_without_send_is_a_noop(self):
        # Some channel-like objects (e.g. categories) lack send(); the
        # helper checks for it rather than blindly calling.
        class _NoSend:
            pass

        post_atmospheric(_NoSend(), "P", "line")
        await asyncio.sleep(0)  # proves no AttributeError

    @pytest.mark.asyncio
    async def test_send_raising_is_swallowed(self):
        # A send-time failure (permissions, rate limit) must never bubble
        # up and abort the dig flow that triggered the flame.
        class _Boom:
            def send(self, content):
                raise RuntimeError("send blew up")

        post_atmospheric(_Boom(), "P", "line")  # must not raise
        await asyncio.sleep(0)

    def test_no_running_loop_is_swallowed(self):
        # Tests and sync code paths invoke this with no event loop; the
        # helper must drop the post instead of raising "no running loop".
        created = []

        class _CapturingChannel:
            def send(self, content):
                coro = _send_body()
                created.append(coro)
                return coro

        async def _send_body():  # pragma: no cover - never awaited by design
            pass

        post_atmospheric(_CapturingChannel(), "P", "line")  # no asyncio loop
        # The coroutine was created but the no-loop branch dropped it
        # unscheduled; close it so pytest sees no "never awaited" warning.
        assert len(created) == 1
        created[0].close()


class TestPostCatastrophic:
    """post_catastrophic posts a randomized cave-in line via post_atmospheric."""

    @pytest.mark.asyncio
    async def test_uses_catastrophic_prefix_and_pool_line(self):
        # The composed post must carry the catastrophic prefix and one of
        # the canonical lines, formatted by post_atmospheric.
        random.seed(7)
        expected_line = pick_catastrophic_line()
        random.seed(7)
        channel = _RecordingChannel()
        post_catastrophic(channel)
        await asyncio.sleep(0)
        assert channel.sent == [f"{CATASTROPHIC_PREFIX} *{expected_line}*"]

    @pytest.mark.asyncio
    async def test_none_channel_is_a_noop(self):
        # Inherits post_atmospheric's None tolerance.
        post_catastrophic(None)
        await asyncio.sleep(0)
