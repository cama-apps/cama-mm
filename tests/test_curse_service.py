"""Tests for CurseService: maybe_flame_and_post gating + cast_curse."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from services.curse_service import WITCH_PREFIX, CurseService


def _make_service(*, stack_count=1, ai_enabled=True, llm_returns="the witch cackles"):
    curse_repo = MagicMock()
    curse_repo.count_active_curses_for_target = MagicMock(return_value=stack_count)
    curse_repo.cast_or_extend = MagicMock(return_value=1234567890)

    flavor = MagicMock()
    flavor.generate_curse_flame = AsyncMock(return_value=llm_returns)

    guild_config_repo = MagicMock()
    guild_config_repo.get_ai_enabled = MagicMock(return_value=ai_enabled)

    service = CurseService(
        curse_repo=curse_repo,
        flavor_text_service=flavor,
        guild_config_repo=guild_config_repo,
    )
    return service, curse_repo, flavor, guild_config_repo


@pytest.mark.asyncio
async def test_cast_curse_calls_repo_and_returns_expiry():
    service, curse_repo, *_ = _make_service()
    expiry = await service.cast_curse(
        caster_id=100, target_id=200, guild_id=12345, days=7
    )
    assert expiry == 1234567890
    curse_repo.cast_or_extend.assert_called_once_with(12345, 100, 200, 7)


@pytest.mark.asyncio
async def test_maybe_flame_returns_silently_when_not_cursed():
    service, _, flavor, _ = _make_service(stack_count=0)
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
async def test_maybe_flame_returns_silently_when_ai_disabled():
    service, _, flavor, _ = _make_service(ai_enabled=False)
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
async def test_maybe_flame_returns_silently_on_llm_none():
    service, _, flavor, _ = _make_service(llm_returns=None)
    channel = AsyncMock()

    # Force the roll to land
    with patch("services.curse_service.random.randint", return_value=1):
        await service.maybe_flame_and_post(
            channel=channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="loss",
        )
    flavor.generate_curse_flame.assert_awaited_once()
    channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_maybe_flame_posts_when_roll_hits_loss():
    service, _, flavor, _ = _make_service()
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
    sent = channel.send.await_args.args[0]
    assert sent.startswith(WITCH_PREFIX)
    assert "the witch cackles" in sent


@pytest.mark.asyncio
async def test_maybe_flame_skips_on_failed_roll():
    service, _, flavor, _ = _make_service()
    channel = AsyncMock()

    # randint > 20% loss threshold
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
async def test_loss_uses_higher_threshold_than_win():
    service, _, _flavor, _ = _make_service()
    # Construct: a value that hits on loss (<=20) but misses on win (<=5)
    with patch("services.curse_service.random.randint", return_value=10):
        loss_channel = AsyncMock()
        win_channel = AsyncMock()
        await service.maybe_flame_and_post(
            channel=loss_channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="loss",
        )
        await service.maybe_flame_and_post(
            channel=win_channel,
            target_id=200,
            guild_id=12345,
            system="match",
            outcome="win",
        )
    loss_channel.send.assert_awaited_once()
    win_channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_swallows_exceptions_silently():
    service, curse_repo, *_ = _make_service()
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
    service, _, flavor, _ = _make_service()
    await service.maybe_flame_and_post(
        channel=None,
        target_id=200,
        guild_id=12345,
        system="match",
        outcome="loss",
    )
    flavor.generate_curse_flame.assert_not_awaited()


@pytest.mark.asyncio
async def test_returns_silently_when_no_flavor_service():
    curse_repo = MagicMock()
    curse_repo.count_active_curses_for_target = MagicMock(return_value=2)
    service = CurseService(
        curse_repo=curse_repo,
        flavor_text_service=None,
        guild_config_repo=None,
    )
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
async def test_passes_stack_count_to_llm():
    service, _, flavor, _ = _make_service(stack_count=3)
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
