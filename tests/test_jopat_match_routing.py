from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

import commands.match as match_module
from commands.match import MatchCommands
from services.jopat_post_match import JopatPostMatchContext
from services.neon_degen_service import NeonDegenService, NeonResult


@pytest.mark.asyncio
async def test_post_match_debrief_uses_base_chance_for_ordinary_match(
    monkeypatch,
) -> None:
    import config

    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
    service = NeonDegenService()
    service._roll = Mock(return_value=False)

    result = await service.on_post_match_debrief(
        10,
        JopatPostMatchContext(winner_name="Winner"),
    )

    assert result is None
    service._roll.assert_called_once_with(0.35)


def test_post_match_debrief_notable_context_uses_middle_chance() -> None:
    context = JopatPostMatchContext(rating_change=25)

    assert NeonDegenService._post_match_chance(context) == 0.55


@pytest.mark.parametrize(
    ("context", "expected"),
    [
        (
            JopatPostMatchContext(
                loser_name="LeveragedLarry", loss=500, leverage=5
            ),
            ("buyback_denied", "LeveragedLarry", 500),
        ),
        (
            JopatPostMatchContext(
                winner_name="UpsetWinner", expected_win_prob=0.25
            ),
            ("odds_anomaly", "UpsetWinner", 25),
        ),
        (
            JopatPostMatchContext(winner_name="HotHand", streak=7),
            ("beyond_godlike", "HotHand", 7),
        ),
        (
            JopatPostMatchContext(winner_name="Climber", rating_change=40),
            ("divine_rapier_position", "Climber", 40),
        ),
        (
            JopatPostMatchContext(winner_name="Bettor", payout=750),
            ("ancient_liquidated", "Bettor", 750),
        ),
        (JopatPostMatchContext(loser_name="ColdHand", streak=-7), None),
        (JopatPostMatchContext(expected_win_prob=0.25), None),
    ],
)
def test_post_match_gif_theme_matches_the_verified_event(
    context: JopatPostMatchContext,
    expected: tuple[str, str, int] | None,
) -> None:
    assert NeonDegenService._post_match_gif_theme(context) == expected


@pytest.mark.asyncio
async def test_post_match_debrief_uses_extreme_chance_and_ansi_fallback(
    monkeypatch,
) -> None:
    import config

    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
    service = NeonDegenService()
    service._roll = Mock(side_effect=[True, False])
    context = JopatPostMatchContext(
        loser_name="LeveragedLarry",
        loss=500,
        leverage=5,
    )

    result = await service.on_post_match_debrief(10, context)

    assert result is not None
    assert result.layer == 2
    assert result.text_block is not None
    assert result.text_block.startswith("```ansi\n")
    assert "JOPA-T" in result.text_block
    assert service._roll.call_args_list[0].args == (0.75,)


@pytest.mark.asyncio
async def test_post_match_debrief_can_promote_extreme_context_to_themed_gif(
    monkeypatch,
) -> None:
    import config

    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
    service = NeonDegenService()
    service._roll = Mock(side_effect=[True, True])

    result = await service.on_post_match_debrief(
        10,
        JopatPostMatchContext(
            winner_name="UpsetWinner",
            expected_win_prob=0.25,
            rating_change=42,
        ),
    )

    assert result is not None
    assert result.layer == 3
    assert result.gif_file is not None
    assert result.gif_file.read(6) in (b"GIF87a", b"GIF89a")


@pytest.mark.asyncio
async def test_post_match_debrief_falls_back_when_ai_fails(monkeypatch) -> None:
    import config

    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
    ai_service = SimpleNamespace(
        complete=AsyncMock(side_effect=RuntimeError("provider unavailable"))
    )
    service = NeonDegenService(ai_service=ai_service)
    service._roll = Mock(return_value=True)

    result = await service.on_post_match_debrief(
        10,
        JopatPostMatchContext(winner_name="Winner", payout=250),
    )

    assert result is not None
    assert result.text_block is not None
    assert result.text_block.startswith("```ansi\n")
    assert "JOPA-T" in result.text_block


@pytest.mark.asyncio
async def test_post_match_debrief_keeps_text_when_gif_rendering_fails(
    monkeypatch,
) -> None:
    import config
    import utils.neon_drawing as drawing

    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
    monkeypatch.setattr(
        drawing,
        "create_post_match_gif",
        Mock(side_effect=RuntimeError("render failed")),
    )
    service = NeonDegenService()
    service._roll = Mock(side_effect=[True, True])

    result = await service.on_post_match_debrief(
        10,
        JopatPostMatchContext(loser_name="Loser", loss=500, leverage=5),
    )

    assert result is not None
    assert result.layer == 2
    assert result.text_block is not None
    assert result.gif_file is None


@pytest.mark.asyncio
async def test_match_hook_keeps_big_win_priority_and_sends_only_one(
    monkeypatch,
) -> None:
    neon = SimpleNamespace(
        on_big_win=AsyncMock(return_value=NeonResult(layer=3, text_block="headline")),
        on_post_match_debrief=AsyncMock(
            return_value=NeonResult(layer=2, text_block="debrief")
        ),
    )
    send_result = AsyncMock()
    monkeypatch.setattr(match_module, "get_neon_service", lambda _bot: neon)
    monkeypatch.setattr(match_module, "send_neon_result", send_result)
    cog = MatchCommands(Mock(), Mock(), Mock(), Mock())
    interaction = Mock()

    await cog._run_neon_match_hooks(
        interaction,
        10,
        [{"discord_id": 1, "amount": 50, "payout": 500}],
        [{"discord_id": 2, "amount": 100}],
        {"match_id": 99, "winning_player_ids": [1]},
    )

    send_result.assert_awaited_once()
    neon.on_post_match_debrief.assert_not_awaited()


@pytest.mark.asyncio
async def test_match_hook_routes_ordinary_match_through_single_jopat_gateway(
    monkeypatch,
) -> None:
    neon = SimpleNamespace(
        on_post_match_debrief=AsyncMock(
            return_value=NeonResult(layer=2, text_block="debrief")
        ),
        _post_match_chance=NeonDegenService._post_match_chance,
    )
    send_result = AsyncMock()
    monkeypatch.setattr(match_module, "get_neon_service", lambda _bot: neon)
    monkeypatch.setattr(match_module, "send_neon_result", send_result)
    match_service = Mock()
    match_service.get_rating_history_for_match.return_value = []
    cog = MatchCommands(Mock(), Mock(), match_service, Mock())
    interaction = Mock()

    await cog._run_neon_match_hooks(
        interaction,
        10,
        [],
        [],
        {"match_id": 99, "winning_player_ids": [1]},
    )

    neon.on_post_match_debrief.assert_awaited_once()
    send_result.assert_awaited_once()


@pytest.mark.asyncio
async def test_match_hook_keeps_selected_facts_attached_to_their_player(
    monkeypatch,
) -> None:
    neon = SimpleNamespace(
        on_post_match_debrief=AsyncMock(
            return_value=NeonResult(layer=2, text_block="debrief")
        ),
        _post_match_chance=NeonDegenService._post_match_chance,
        _get_degen_score=Mock(return_value=None),
    )
    send_result = AsyncMock()
    monkeypatch.setattr(match_module, "get_neon_service", lambda _bot: neon)
    monkeypatch.setattr(match_module, "send_neon_result", send_result)
    match_service = Mock()
    match_service.get_rating_history_for_match.return_value = [
        {
            "discord_id": 1,
            "rating": 1550,
            "rating_before": 1500,
            "expected_team_win_prob": 0.25,
        }
    ]
    player_service = Mock()
    player_service.get_balance.return_value = 100
    cog = MatchCommands(Mock(), Mock(), match_service, player_service)

    await cog._run_neon_match_hooks(
        Mock(),
        10,
        [],
        [
            {
                "discord_id": 2,
                "amount": 100,
                "effective_bet": 500,
                "leverage": 5,
            }
        ],
        {"match_id": 99, "winning_player_ids": [1]},
    )

    call = neon.on_post_match_debrief.await_args
    context = call.args[1]
    assert context.loss == 500
    assert context.leverage == 5
    assert context.rating_change is None
    assert call.kwargs == {"winner_id": None, "loser_id": 2}


@pytest.mark.asyncio
async def test_enrichment_rolls_once_and_returns_at_most_one_callout(
    monkeypatch,
) -> None:
    import config

    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
    service = NeonDegenService()
    service._roll = Mock(return_value=True)
    winners = [
        {
            "discord_id": player_id,
            "hero_id": 1,
            "kills": kills,
            "deaths": 2,
            "assists": 10,
            "gpm": 500 + kills,
            "fantasy_points": 20 + kills,
        }
        for player_id, kills in enumerate(range(5, 10), start=1)
    ]

    results = await service.on_match_enriched(10, winners, [])

    assert len(results) == 1
    service._roll.assert_called_once()


@pytest.mark.asyncio
async def test_enrichment_uses_the_configured_per_match_chance(
    monkeypatch,
) -> None:
    import config
    import services.neon_degen_service as neon_module

    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
    monkeypatch.setattr(neon_module, "NEON_MVP_CHANCE", 0.23, raising=False)
    service = NeonDegenService()
    service._roll = Mock(return_value=False)

    results = await service.on_match_enriched(
        10,
        [{"discord_id": 1, "kills": 8, "deaths": 2, "assists": 10}],
        [],
    )

    assert results == []
    service._roll.assert_called_once_with(0.23)


@pytest.mark.asyncio
async def test_enrichment_can_roast_a_genuinely_extreme_loser(monkeypatch) -> None:
    import config

    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
    service = NeonDegenService()
    service._roll = Mock(return_value=True)
    loser = {
        "discord_id": 44,
        "hero_id": 2,
        "kills": 0,
        "deaths": 16,
        "assists": 2,
        "gpm": 180,
        "xpm": 210,
    }

    results = await service.on_match_enriched(10, [], [loser])

    assert len(results) == 1
    assert results[0].text_block is not None
    assert "Client-44" in results[0].text_block or "<@44>" in results[0].text_block


@pytest.mark.asyncio
async def test_enrichment_does_not_roast_an_ordinary_loser(monkeypatch) -> None:
    import config

    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
    service = NeonDegenService()
    service._roll = Mock(return_value=True)
    loser = {
        "discord_id": 45,
        "hero_id": 2,
        "kills": 0,
        "deaths": 6,
        "assists": 1,
        "gpm": 410,
        "xpm": 430,
    }

    results = await service.on_match_enriched(10, [], [loser])

    assert results == []
    service._roll.assert_not_called()
