"""Performance regressions for the profile Heroes tab."""

import asyncio
from io import BytesIO
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

import commands.profile as profile_module
import utils.mana_display as mana_display_module
from commands.profile import ProfileCommands
from domain.models.player import Player


def _track_thread_waves(monkeypatch):
    state = {"active": 0, "peak": 0}

    async def enter(function):
        state["active"] += 1
        state["peak"] = max(state["peak"], state["active"])
        await asyncio.sleep(0)
        try:
            return function()
        finally:
            state["active"] -= 1

    async def tracked_to_thread(function, /, *args, **kwargs):
        return await enter(lambda: function(*args, **kwargs))

    monkeypatch.setattr(profile_module.asyncio, "to_thread", tracked_to_thread)
    return state, enter


@pytest.mark.asyncio
async def test_heroes_tab_runs_independent_reads_and_drawing_concurrently(
    monkeypatch,
):
    player_repo = SimpleNamespace(get_by_id=MagicMock(return_value=object()))
    hero_stats = [
        {
            "hero_id": 1,
            "games": 2,
            "wins": 1,
            "avg_kills": 5.0,
            "avg_deaths": 4.0,
            "avg_assists": 10.0,
            "avg_gpm": 500.0,
        }
    ]
    match_repo = SimpleNamespace(
        get_player_enriched_match_count=MagicMock(return_value=2),
        get_player_hero_detailed_stats=MagicMock(return_value=hero_stats),
        get_player_overall_hero_stats=MagicMock(
            return_value={
                "total_games": 2,
                "avg_kills": 5.0,
                "avg_deaths": 4.0,
                "avg_assists": 10.0,
                "avg_gpm": 500.0,
                "total_obs": 0,
                "total_sens": 0,
            }
        ),
        get_player_lane_stats=MagicMock(return_value=[]),
        get_player_ward_stats_by_lane=MagicMock(return_value=[]),
        get_player_nemesis_heroes=MagicMock(return_value=[]),
        get_player_easiest_opponents=MagicMock(return_value=[]),
        get_player_best_hero_synergies=MagicMock(return_value=[]),
        get_player_hero_vs_opponent_heroes=MagicMock(return_value=[]),
        get_player_hero_lane_performance=MagicMock(return_value=[]),
    )
    cog = ProfileCommands(
        SimpleNamespace(player_repo=player_repo, match_repo=match_repo)
    )

    state, _ = _track_thread_waves(monkeypatch)
    monkeypatch.setattr(
        profile_module,
        "draw_hero_performance_chart",
        MagicMock(return_value=BytesIO(b"chart")),
    )

    embed, files = await cog._build_heroes_embed(
        SimpleNamespace(display_name="Player"),
        123,
        guild_id=456,
    )

    try:
        # Drawing plus eight independent aggregate reads share the second wave.
        assert state["peak"] == 9
        assert embed.title == "Profile: Player > Heroes"
        assert [file.filename for file in files] == ["hero_chart.png"]
        for method in (
            match_repo.get_player_overall_hero_stats,
            match_repo.get_player_lane_stats,
            match_repo.get_player_ward_stats_by_lane,
            match_repo.get_player_nemesis_heroes,
            match_repo.get_player_easiest_opponents,
            match_repo.get_player_best_hero_synergies,
            match_repo.get_player_hero_vs_opponent_heroes,
            match_repo.get_player_hero_lane_performance,
        ):
            method.assert_called_once()
    finally:
        for file in files:
            file.close()


@pytest.mark.asyncio
async def test_overview_loads_badge_bankruptcy_and_hero_stats_concurrently(
    monkeypatch,
):
    state, enter = _track_thread_waves(monkeypatch)

    async def resolve_badge(*args):
        return await enter(lambda: "")

    monkeypatch.setattr(mana_display_module, "resolve_mana_badge", resolve_badge)
    player = SimpleNamespace(
        wins=0,
        losses=0,
        preferred_roles=[],
        main_role=None,
        preferred_region=None,
        inferred_region=None,
    )
    player_service = SimpleNamespace(
        get_stats=MagicMock(
            return_value={
                "player": player,
                "cama_rating": None,
                "uncertainty": 100.0,
                "win_rate": 0.0,
                "jopacoin_balance": 100,
            }
        )
    )
    bankruptcy_service = SimpleNamespace(
        get_state=MagicMock(
            return_value=SimpleNamespace(penalty_games_remaining=0)
        )
    )
    match_repo = SimpleNamespace(
        get_player_hero_stats=MagicMock(return_value={})
    )
    cog = ProfileCommands(
        SimpleNamespace(
            player_service=player_service,
            bankruptcy_service=bankruptcy_service,
            match_repo=match_repo,
        )
    )

    embed, chart_file = await cog._build_overview_embed(
        SimpleNamespace(display_name="Player"),
        123,
        guild_id=456,
    )

    assert chart_file is None
    assert embed.title == "Profile: Player"
    assert state["peak"] == 3


@pytest.mark.asyncio
async def test_predictions_loads_stats_and_positions_concurrently(monkeypatch):
    state, _ = _track_thread_waves(monkeypatch)
    prediction_service = SimpleNamespace(
        get_user_orderbook_stats=MagicMock(
            return_value={
                "resolved_markets": 0,
                "realized_pnl": 0,
                "wins": 0,
                "losses": 0,
            }
        ),
        get_user_open_positions=MagicMock(return_value=[]),
    )
    cog = ProfileCommands(
        SimpleNamespace(prediction_service=prediction_service)
    )

    embed, chart_file = await cog._build_predictions_embed(
        SimpleNamespace(display_name="Player"),
        123,
        guild_id=456,
    )

    assert chart_file is None
    assert "No prediction market activity" in embed.description
    assert state["peak"] == 2


@pytest.mark.asyncio
async def test_gambling_loads_balance_impact_and_chart_series_concurrently(
    monkeypatch,
):
    state, _ = _track_thread_waves(monkeypatch)
    degen = SimpleNamespace(
        total=0,
        emoji="🎲",
        title="Casual",
        flavor_texts=[],
        tagline="Just visiting",
        max_leverage_score=0,
        bet_size_score=0,
        debt_depth_score=0,
        bankruptcy_score=0,
        frequency_score=0,
        loss_chase_score=0,
        negative_loan_bonus=0,
    )
    stats = SimpleNamespace(
        net_pnl=0,
        degen_score=degen,
        roi=0.0,
        wins=0,
        losses=0,
        win_rate=0.0,
        total_bets=1,
        total_wagered=10,
        avg_bet_size=10.0,
        leverage_distribution={},
        current_streak=0,
        best_streak=0,
        worst_streak=0,
        peak_pnl=0,
        trough_pnl=0,
        biggest_win=0,
        biggest_loss=0,
        matches_played=0,
        paper_hands_count=0,
    )
    gambling_service = SimpleNamespace(
        get_player_stats=MagicMock(return_value=stats),
        get_betting_impact_stats=MagicMock(return_value=None),
        get_cumulative_pnl_series=MagicMock(return_value=[]),
    )
    player_service = SimpleNamespace(
        get_player=MagicMock(
            return_value=SimpleNamespace(jopacoin_balance=100)
        )
    )
    cog = ProfileCommands(
        SimpleNamespace(
            gambling_stats_service=gambling_service,
            player_service=player_service,
        )
    )

    embed, chart_file = await cog._build_gambling_embed(
        SimpleNamespace(display_name="Player"),
        123,
        guild_id=456,
    )

    assert chart_file is None
    assert embed.title == "Profile: Player > Gambling"
    assert state["peak"] == 3


@pytest.mark.asyncio
async def test_rating_loads_population_and_history_concurrently(monkeypatch):
    state, _ = _track_thread_waves(monkeypatch)
    player = Player(
        name="Player",
        initial_mmr=6000,
        glicko_rating=1500.0,
        glicko_rd=100.0,
        glicko_volatility=0.06,
    )
    player_repo = SimpleNamespace(
        get_by_id=MagicMock(return_value=player),
        get_all=MagicMock(return_value=[player]),
    )
    match_repo = SimpleNamespace(
        get_player_rating_history_detailed=MagicMock(return_value=[])
    )
    cog = ProfileCommands(
        SimpleNamespace(player_repo=player_repo, match_repo=match_repo)
    )

    embed, chart_file = await cog._build_rating_embed(
        SimpleNamespace(display_name="Player"),
        123,
        guild_id=456,
    )

    assert chart_file is None
    assert embed.title == "Profile: Player > Rating"
    assert state["peak"] == 2
    match_repo.get_player_rating_history_detailed.assert_called_once_with(
        123,
        456,
        limit=999,
    )
    player_repo.get_all.assert_called_once_with(456)
