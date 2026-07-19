from unittest.mock import AsyncMock, Mock

import pytest

from services.duel_flavor_service import (
    HERALD_VOICES,
    PROMPT_CONSTRAINTS,
    SYSTEM_PROMPT,
    DuelFlavorEvent,
    DuelFlavorService,
)


async def test_flavor_uses_dedicated_prompt_and_sanitizes_output():
    ai = Mock()
    ai.complete = AsyncMock(
        return_value="@everyone The court trembles.\n<@&123> @here Roshan approves."
    )
    config = Mock(get_ai_enabled=Mock(return_value=True))
    service = DuelFlavorService(ai, config, rng=Mock(choice=lambda values: values[0]))

    result = await service.generate(
        DuelFlavorEvent.ISSUED,
        42,
        {"challenger": "A<@1>", "recipient": "B", "wager": 500},
    )

    assert "@everyone" not in result
    assert "@here" not in result
    assert "@" not in result
    assert "\n" not in result
    system_prompt = ai.complete.await_args.kwargs["system_prompt"]
    assert "Game of Thrones" not in system_prompt
    assert "Tales of Dunk and Egg" in system_prompt
    assert "Do not quote or imitate" in system_prompt
    assert system_prompt == f"{SYSTEM_PROMPT} {HERALD_VOICES[0]} {PROMPT_CONSTRAINTS}"
    assert system_prompt.endswith(PROMPT_CONSTRAINTS)
    assert len(result) <= 300


async def test_flavor_falls_back_when_ai_is_missing():
    service = DuelFlavorService(None, None, rng=Mock(choice=lambda values: values[0]))

    result = await service.generate(DuelFlavorEvent.EXPIRED, 42, {})

    assert result
    assert len(result) <= 300


async def test_flavor_falls_back_without_calling_ai_when_guild_ai_is_disabled():
    ai = Mock(complete=AsyncMock(return_value="unused"))
    config = Mock(get_ai_enabled=Mock(return_value=False))
    service = DuelFlavorService(ai, config, rng=Mock(choice=lambda values: values[0]))

    result = await service.generate(DuelFlavorEvent.DECLINED, 42, {})

    assert result
    ai.complete.assert_not_awaited()


@pytest.mark.parametrize("provider_result", [None, " \n\t\r "])
async def test_flavor_falls_back_when_provider_returns_no_usable_text(
    provider_result,
):
    ai = Mock(complete=AsyncMock(return_value=provider_result))
    config = Mock(get_ai_enabled=Mock(return_value=True))
    service = DuelFlavorService(ai, config, rng=Mock(choice=lambda values: values[0]))

    result = await service.generate(DuelFlavorEvent.VOIDED, 42, {})

    assert result


async def test_flavor_falls_back_when_provider_raises():
    ai = Mock(complete=AsyncMock(side_effect=RuntimeError("provider unavailable")))
    config = Mock(get_ai_enabled=Mock(return_value=True))
    service = DuelFlavorService(ai, config, rng=Mock(choice=lambda values: values[0]))

    result = await service.generate(DuelFlavorEvent.RESOLVED, 42, {})

    assert result


async def test_flavor_sanitizes_and_caps_details_before_prompt_interpolation():
    ai = Mock(complete=AsyncMock(return_value="The lists are opened."))
    config = Mock(get_ai_enabled=Mock(return_value=True))
    service = DuelFlavorService(ai, config)
    dangerous_name = "A<@1>`\n\r\t" + ("x" * 100) + "SECRET"

    await service.generate(
        DuelFlavorEvent.REMINDER,
        42,
        {"challenger": dangerous_name},
    )

    prompt = ai.complete.await_args.args[0]
    assert all(character not in prompt for character in "<>@`\n\r\t")
    assert "SECRET" not in prompt


async def test_every_lifecycle_event_has_a_distinct_fallback():
    service = DuelFlavorService(None, None, rng=Mock(choice=lambda values: values[0]))

    results = {
        event: await service.generate(event, 42, {}) for event in DuelFlavorEvent
    }

    assert set(results) == set(DuelFlavorEvent)
    assert all(results.values())
    assert len(set(results.values())) == len(DuelFlavorEvent)
    assert all(len(result) <= 300 for result in results.values())
