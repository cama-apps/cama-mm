from types import SimpleNamespace
from unittest.mock import MagicMock

import discord
import pytest

from commands.profile import ProfileCommands
from commands.profile_helpers.gambling import add_auto_bet_performance_field
from utils.embed_safety import EMBED_LIMITS, validate_embed


def _group(*, bets: int, wins: int, wagered: int, pnl: int) -> SimpleNamespace:
    return SimpleNamespace(
        bet_count=bets,
        wins=wins,
        total_wagered=wagered,
        net_pnl=pnl,
    )


def _performance(target_count: int) -> SimpleNamespace:
    targets = [
        SimpleNamespace(
            target_id=9_000_000_000_000_000_000 + index,
            directions=("long",) if index % 2 == 0 else ("short",),
            bet_count=12 + index,
            wins=7 + index // 2,
            total_wagered=12_345 + index,
            net_pnl=(1 if index % 3 else -1) * (2_345 + index),
        )
        for index in range(target_count)
    ]
    return SimpleNamespace(
        total=_group(bets=180, wins=101, wagered=987_654, pnl=12_345),
        generic=_group(bets=40, wins=19, wagered=123_456, pnl=-7_890),
        targets=targets,
        arbitrage=_group(bets=13, wins=8, wagered=54_321, pnl=4_567),
    )


def _gambling_stats(performance: SimpleNamespace) -> SimpleNamespace:
    degen = SimpleNamespace(
        total=42,
        emoji="🔥",
        title="Committed",
        flavor_texts=["high roller"],
        tagline="The house knows your name",
        max_leverage_score=10,
        bet_size_score=8,
        debt_depth_score=0,
        bankruptcy_score=0,
        frequency_score=4,
        loss_chase_score=1,
        negative_loan_bonus=0,
    )
    return SimpleNamespace(
        net_pnl=123,
        degen_score=degen,
        roi=0.12,
        wins=20,
        losses=15,
        win_rate=20 / 35,
        total_bets=35,
        total_wagered=1_025,
        avg_bet_size=29.3,
        leverage_distribution={1: 30, 2: 5},
        current_streak=2,
        best_streak=5,
        worst_streak=-4,
        peak_pnl=200,
        trough_pnl=-80,
        biggest_win=60,
        biggest_loss=-40,
        matches_played=20,
        paper_hands_count=3,
        auto_bet_performance=performance,
    )


@pytest.mark.asyncio
async def test_gambling_profile_renders_budgeted_auto_bet_pnl_with_omitted_targets():
    performance = _performance(80)
    gambling_service = SimpleNamespace(
        get_player_stats=MagicMock(return_value=_gambling_stats(performance)),
        get_betting_impact_stats=MagicMock(return_value=None),
        get_cumulative_pnl_series=MagicMock(return_value=[]),
    )
    player_service = SimpleNamespace(
        get_player=MagicMock(return_value=SimpleNamespace(jopacoin_balance=500))
    )
    cog = ProfileCommands(
        SimpleNamespace(
            gambling_stats_service=gambling_service,
            player_service=player_service,
        )
    )

    embed, chart_file = await cog._build_gambling_embed(
        SimpleNamespace(display_name="Investor"),
        123,
        guild_id=456,
    )

    assert chart_file is None
    assert embed.fields[-1].name == "Auto-Bet P&L"
    value = embed.fields[-1].value
    assert "**Auto total:** +12,345 JC" in value
    assert "**Generic auto:** -7,890 JC" in value
    assert "**Arbitrage hedges:** +4,567 JC" in value
    assert "<@9000000000000000000> LONG" in value
    assert "<@9000000000000000001> SHORT" in value
    assert "more targets omitted" in value
    assert len(value) <= EMBED_LIMITS["field_value"]
    assert len(embed) <= EMBED_LIMITS["total"]
    assert validate_embed(embed) == []


def test_auto_bet_field_respects_remaining_total_embed_budget():
    embed = discord.Embed(description="x" * EMBED_LIMITS["description"])
    embed.add_field(name="Existing A", value="a" * 900, inline=False)
    embed.add_field(name="Existing B", value="b" * 800, inline=False)

    add_auto_bet_performance_field(embed, _performance(20))

    assert embed.fields[-1].name == "Auto-Bet P&L"
    assert "omitted" in embed.fields[-1].value
    assert len(embed) <= EMBED_LIMITS["total"]
    assert validate_embed(embed) == []


def test_auto_bet_field_is_omitted_without_automatic_bet_history():
    performance = SimpleNamespace(
        total=_group(bets=0, wins=0, wagered=0, pnl=0),
        generic=_group(bets=0, wins=0, wagered=0, pnl=0),
        targets=[],
        arbitrage=_group(bets=0, wins=0, wagered=0, pnl=0),
    )
    embed = discord.Embed(title="Gambling")

    add_auto_bet_performance_field(embed, performance)

    assert embed.fields == []
