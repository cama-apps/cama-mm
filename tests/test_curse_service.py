"""Tests for CurseService: maybe_flame_and_post gating + cast_curse.

The hex fires a pure-PIL witchfire GIF (no AI dependency) only on losses, then
optionally rides an LLM taunt as the caption when a flavor service is present.
"""

from __future__ import annotations

import asyncio
import io
from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

from services.curse_service import WITCH_PREFIX, CurseService


@pytest.fixture(autouse=True)
def _fast_curse(monkeypatch):
    """Stub the (slow) PIL witchfire GIF and disable the cooldown by default.

    The cooldown tests re-enable it explicitly. The real GIF generator has its
    own rendering test in test_neon_degen.py; here we only exercise the send path.
    """
    monkeypatch.setattr(
        "utils.neon_drawing.create_witch_curse_gif",
        lambda *a, **k: io.BytesIO(b"GIF89a"),
    )
    monkeypatch.setattr("services.curse_service.WITCHS_CURSE_COOLDOWN_SECONDS", 0)


def _make_service(*, stack_count=1, llm_returns="the witch cackles"):
    curse_repo = MagicMock()
    curse_repo.count_active_curses_for_target = MagicMock(return_value=stack_count)
    curse_repo.cast_or_extend = MagicMock(return_value=1234567890)

    flavor = MagicMock()
    flavor.generate_curse_flame = AsyncMock(return_value=llm_returns)

    service = CurseService(curse_repo=curse_repo, flavor_text_service=flavor)
    return service, curse_repo, flavor


@pytest.mark.asyncio
async def test_cast_curse_calls_repo_and_returns_expiry():
    service, curse_repo, _ = _make_service()
    expiry = await service.cast_curse(
        caster_id=100, target_id=200, guild_id=12345, days=7
    )
    assert expiry == 1234567890
    curse_repo.cast_or_extend.assert_called_once_with(12345, 100, 200, 7)


@pytest.mark.asyncio
async def test_maybe_flame_returns_silently_when_not_cursed():
    service, _, flavor = _make_service(stack_count=0)
    channel = AsyncMock()

    await service.maybe_flame_and_post(
        channel=channel,
        target_id=200,
        guild_id=12345,
        system="match",
        outcome="loss",
    )
    flavor.generate_curse_flame.assert_not_awaited()
    channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_win_and_neutral_never_fire():
    # The hex only bites when the victim is down. Even a guaranteed roll must not
    # fire on wins/neutral — and the filter is cheap enough to skip the DB read.
    service, curse_repo, flavor = _make_service()

    with patch("services.curse_service.random.randint", return_value=1):
        for outcome in ("win", "neutral"):
            channel = AsyncMock()
            await service.maybe_flame_and_post(
                channel=channel,
                target_id=200,
                guild_id=12345,
                system="match",
                outcome=outcome,
            )
            channel.send.assert_not_awaited()
    curse_repo.count_active_curses_for_target.assert_not_called()
    flavor.generate_curse_flame.assert_not_awaited()


@pytest.mark.asyncio
async def test_posts_gif_and_caption_when_roll_hits_loss():
    service, _, flavor = _make_service()
    channel = AsyncMock()

    with patch("services.curse_service.random.randint", return_value=1):
        await service.maybe_flame_and_post(
            channel=channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="loss",
        )
    flavor.generate_curse_flame.assert_awaited_once()
    channel.send.assert_awaited_once()
    call = channel.send.await_args
    # caption is the leading positional arg; the witchfire GIF rides along as a file
    assert call.args[0].startswith(WITCH_PREFIX)
    assert "the witch cackles" in call.args[0]
    assert isinstance(call.kwargs["file"], discord.File)
    assert call.kwargs["delete_after"] == 90


@pytest.mark.asyncio
async def test_fires_gif_without_caption_when_llm_silent():
    # AI is on but the model returns nothing → the GIF still erupts, just untexted.
    service, _, flavor = _make_service(llm_returns=None)
    channel = AsyncMock()

    with patch("services.curse_service.random.randint", return_value=1):
        await service.maybe_flame_and_post(
            channel=channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="loss",
        )
    flavor.generate_curse_flame.assert_awaited_once()
    channel.send.assert_awaited_once()
    call = channel.send.await_args
    assert call.args == ()  # no caption
    assert isinstance(call.kwargs["file"], discord.File)


@pytest.mark.asyncio
async def test_fires_gif_with_ai_off_and_no_flavor_service():
    # Regression: before the fix, "no Cerebras key" (flavor_text_service is None)
    # made the curse silently no-op forever. The witchfire GIF must now fire with
    # no LLM involved at all.
    curse_repo = MagicMock()
    curse_repo.count_active_curses_for_target = MagicMock(return_value=2)
    service = CurseService(curse_repo=curse_repo, flavor_text_service=None)
    channel = AsyncMock()

    with patch("services.curse_service.random.randint", return_value=1):
        await service.maybe_flame_and_post(
            channel=channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="loss",
        )
    channel.send.assert_awaited_once()
    call = channel.send.await_args
    assert call.args == ()  # no caption without a flavor service
    assert isinstance(call.kwargs["file"], discord.File)


@pytest.mark.asyncio
async def test_maybe_flame_skips_on_failed_roll():
    service, _, flavor = _make_service()
    channel = AsyncMock()

    # randint above the 25% loss threshold → no fire (gates the GIF, not just text)
    with patch("services.curse_service.random.randint", return_value=99):
        await service.maybe_flame_and_post(
            channel=channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="loss",
        )
    flavor.generate_curse_flame.assert_not_awaited()
    channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_cooldown_suppresses_second_fire(monkeypatch):
    # With the cooldown active, a tilting (repeatedly-losing) victim sees at most
    # one witchfire per window, no matter how many losses they rack up.
    monkeypatch.setattr("services.curse_service.WITCHS_CURSE_COOLDOWN_SECONDS", 3600)
    service, _, _ = _make_service()
    channel = AsyncMock()

    with patch("services.curse_service.random.randint", return_value=1):
        for _ in range(2):
            await service.maybe_flame_and_post(
                channel=channel,
                target_id=200,
                guild_id=12345,
                system="match",
                outcome="loss",
            )
    channel.send.assert_awaited_once()


@pytest.mark.asyncio
async def test_concurrent_losses_fire_once(monkeypatch):
    # Two near-simultaneous loss events for the same target (e.g. losing a match
    # you also bet on → match + bet hooks both spawn a task) must not double-fire.
    # The cooldown is committed before the first await, so the second task sees it.
    monkeypatch.setattr("services.curse_service.WITCHS_CURSE_COOLDOWN_SECONDS", 3600)
    service, _, _ = _make_service()
    channel = AsyncMock()

    with patch("services.curse_service.random.randint", return_value=1):
        await asyncio.gather(
            *[
                service.maybe_flame_and_post(
                    channel=channel,
                    target_id=200,
                    guild_id=12345,
                    system=sys,
                    outcome="loss",
                )
                for sys in ("match", "bet")
            ]
        )
    assert channel.send.await_count == 1


@pytest.mark.asyncio
async def test_cooldown_expires_and_refires(monkeypatch):
    # Once the window passes, the next loss is allowed to fire again.
    monkeypatch.setattr("services.curse_service.WITCHS_CURSE_COOLDOWN_SECONDS", 3600)
    service, _, _ = _make_service()
    channel = AsyncMock()

    fake_now = [1_000_000.0]
    monkeypatch.setattr("services.curse_service.time.time", lambda: fake_now[0])

    with patch("services.curse_service.random.randint", return_value=1):
        await service.maybe_flame_and_post(
            channel=channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="loss",
        )
        fake_now[0] += 3601  # advance past the cooldown window
        await service.maybe_flame_and_post(
            channel=channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="loss",
        )
    assert channel.send.await_count == 2


@pytest.mark.asyncio
async def test_swallows_exceptions_silently():
    service, curse_repo, _ = _make_service()
    curse_repo.count_active_curses_for_target = MagicMock(side_effect=RuntimeError("db down"))
    channel = AsyncMock()

    await service.maybe_flame_and_post(
        channel=channel,
        target_id=200,
        guild_id=12345,
        system="match",
        outcome="loss",
    )
    channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_returns_silently_when_channel_is_none():
    service, _, flavor = _make_service()
    await service.maybe_flame_and_post(
        channel=None,
        target_id=200,
        guild_id=12345,
        system="match",
        outcome="loss",
    )
    flavor.generate_curse_flame.assert_not_awaited()


@pytest.mark.asyncio
async def test_passes_stack_count_to_llm():
    service, _, flavor = _make_service(stack_count=3)
    channel = AsyncMock()
    with patch("services.curse_service.random.randint", return_value=1):
        await service.maybe_flame_and_post(
            channel=channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="loss",
            event_context={"hero": "Riki"},
            target_display_name="dane",
        )
    call = flavor.generate_curse_flame.await_args
    assert call.kwargs["stack_count"] == 3
    assert call.kwargs["event_context"] == {"hero": "Riki"}
    assert call.kwargs["target_display_name"] == "dane"
