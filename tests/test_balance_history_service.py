"""Unit tests for ``BalanceHistoryService`` — merges seven event sources into one series."""

from __future__ import annotations

from unittest.mock import MagicMock

from services.balance_history_service import (
    SOURCE_BETS,
    SOURCE_BONUS,
    SOURCE_DISBURSE,
    SOURCE_DOUBLE_OR_NOTHING,
    SOURCE_PREDICTIONS,
    SOURCE_TIPS,
    SOURCE_WHEEL,
    BalanceHistoryService,
)


def _build_service(**overrides):
    repos = {
        "bet_repo": MagicMock(),
        "match_repo": MagicMock(),
        "player_repo": MagicMock(),
        "prediction_repo": MagicMock(),
        "disburse_repo": MagicMock(),
        "tip_repo": MagicMock(),
    }
    # Every repo returns an empty list by default so tests only populate what they need.
    repos["bet_repo"].get_player_bet_history.return_value = []
    repos["match_repo"].get_player_bonus_events.return_value = []
    repos["player_repo"].get_wheel_spin_history.return_value = []
    repos["player_repo"].get_double_or_nothing_history.return_value = []
    repos["prediction_repo"].get_player_prediction_history.return_value = []
    repos["disburse_repo"].get_recipient_history.return_value = []
    repos["tip_repo"].get_all_tips_for_user.return_value = []
    repos.update(overrides)
    return BalanceHistoryService(**repos), repos


def test_empty_history_returns_empty_series_and_totals():
    svc, _ = _build_service()
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert series == []
    assert totals == {}


def test_single_source_bet_only_series():
    svc, repos = _build_service()
    repos["bet_repo"].get_player_bet_history.return_value = [
        {"bet_time": 1000, "profit": 50, "outcome": "won", "amount": 10, "leverage": 1, "match_id": 1},
        {"bet_time": 2000, "profit": -10, "outcome": "lost", "amount": 10, "leverage": 1, "match_id": 2},
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert len(series) == 2
    assert series[0][0] == 1
    assert series[0][1] == 50        # cumulative after first event
    assert series[1][1] == 40        # 50 + (-10)
    assert totals == {SOURCE_BETS: 40}


def test_multi_source_merge_preserves_time_order():
    svc, repos = _build_service()
    repos["bet_repo"].get_player_bet_history.return_value = [
        {"bet_time": 3000, "profit": 100, "outcome": "won", "amount": 50, "leverage": 1, "match_id": 1},
    ]
    repos["player_repo"].get_wheel_spin_history.return_value = [
        {"spin_time": 1000, "result": 20},
    ]
    repos["tip_repo"].get_all_tips_for_user.return_value = [
        {"timestamp": 2000, "amount": 30, "fee": 0, "direction": "received",
         "sender_id": 2, "recipient_id": 1},
    ]

    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)

    sources_in_order = [info["source"] for _, _, info in series]
    assert sources_in_order == [SOURCE_WHEEL, SOURCE_TIPS, SOURCE_BETS]
    # Cumulative trajectory: 20 → 50 → 150
    assert [cum for _, cum, _ in series] == [20, 50, 150]
    assert totals == {SOURCE_WHEEL: 20, SOURCE_TIPS: 30, SOURCE_BETS: 100}


def test_per_source_totals_exclude_zero_net_sources():
    svc, repos = _build_service()
    # Bet breaks even across two bets; wheel contributes a net positive
    repos["bet_repo"].get_player_bet_history.return_value = [
        {"bet_time": 1000, "profit": 50, "outcome": "won", "amount": 10, "leverage": 1, "match_id": 1},
        {"bet_time": 2000, "profit": -50, "outcome": "lost", "amount": 10, "leverage": 1, "match_id": 2},
    ]
    repos["player_repo"].get_wheel_spin_history.return_value = [
        {"spin_time": 3000, "result": 15},
    ]
    _series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert SOURCE_BETS not in totals          # net zero → excluded
    assert totals == {SOURCE_WHEEL: 15}


def test_prediction_emits_net_event_per_settlement():
    svc, repos = _build_service()
    repos["prediction_repo"].get_player_prediction_history.return_value = [
        # Won prediction: staked 20, payout 40 → +20
        {"settle_time": 1000, "total_amount": 20, "payout": 40, "position": "yes", "status": "resolved", "prediction_id": 1},
        # Lost prediction: staked 30, payout 0 → -30
        {"settle_time": 2000, "total_amount": 30, "payout": 0, "position": "no", "status": "resolved", "prediction_id": 2},
        # Cancelled prediction fully refunded: staked 10, payout 10 → 0 (dropped)
        {"settle_time": 3000, "total_amount": 10, "payout": 10, "position": "yes", "status": "cancelled", "prediction_id": 3},
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert len(series) == 2                    # cancelled refund dropped
    assert [info["delta"] for _, _, info in series] == [20, -30]
    assert totals == {SOURCE_PREDICTIONS: -10}


def test_wheel_lose_a_turn_results_are_skipped():
    svc, repos = _build_service()
    repos["player_repo"].get_wheel_spin_history.return_value = [
        {"spin_time": 1000, "result": 0},      # lose a turn — ignore
        {"spin_time": 2000, "result": 25},     # actual win
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert len(series) == 1
    assert series[0][1] == 25
    assert totals == {SOURCE_WHEEL: 25}


def test_double_or_nothing_delta_from_balance_math():
    svc, repos = _build_service()
    repos["player_repo"].get_double_or_nothing_history.return_value = [
        # balance_before=100 (after cost deducted), cost=0, balance_after=200, won=1
        # original balance = 100+0=100, delta = 200-100 = +100
        {"spin_time": 1000, "cost": 0, "balance_before": 100, "balance_after": 200, "won": 1},
        # balance_before=50, cost=0, balance_after=0, won=0 → delta = -50
        {"spin_time": 2000, "cost": 0, "balance_before": 50, "balance_after": 0, "won": 0},
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [100, -50]
    assert totals == {SOURCE_DOUBLE_OR_NOTHING: 50}


def test_tip_sent_debits_amount_plus_fee_and_received_credits_amount():
    svc, repos = _build_service()
    repos["tip_repo"].get_all_tips_for_user.return_value = [
        {"timestamp": 1000, "amount": 20, "fee": 2, "direction": "sent",
         "sender_id": 1, "recipient_id": 2},      # -22
        {"timestamp": 2000, "amount": 50, "fee": 5, "direction": "received",
         "sender_id": 3, "recipient_id": 1},      # +50 (fee went to fund)
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [-22, 50]
    assert totals == {SOURCE_TIPS: 28}


def test_self_tip_collapses_to_fee_only():
    """A self-tip's real balance impact is just the fee — the amount goes out and comes back."""
    svc, repos = _build_service()
    repos["tip_repo"].get_all_tips_for_user.return_value = [
        {"timestamp": 1000, "amount": 100, "fee": 5, "direction": "sent",
         "sender_id": 1, "recipient_id": 1},      # self-tip → -5, not -105
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [-5]
    assert totals == {SOURCE_TIPS: -5}


def test_disbursement_events_credit_recipient_amount():
    svc, repos = _build_service()
    repos["disburse_repo"].get_recipient_history.return_value = [
        {"disbursed_at": 1000, "amount": 42, "method": "even"},
        {"disbursed_at": 2000, "amount": 0, "method": "even"},   # zero — dropped
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert len(series) == 1
    assert series[0][1] == 42
    assert totals == {SOURCE_DISBURSE: 42}


def test_match_bonuses_collapse_to_one_event_per_match():
    svc, repos = _build_service()
    # Two matches: one win, one loss. JOPACOIN_PER_GAME=1, JOPACOIN_WIN_REWARD=2 (defaults).
    repos["match_repo"].get_player_bonus_events.return_value = [
        {"match_id": 1, "match_time": 1000, "won": True},   # 1 + 2 = 3
        {"match_id": 2, "match_time": 2000, "won": False},  # 1 + 0 = 1
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert len(series) == 2
    deltas = [info["delta"] for _, _, info in series]
    assert sum(deltas) == 4
    assert totals.get(SOURCE_BONUS) == 4
    # First event carries a detail breakdown with both components
    assert series[0][2]["detail"]["components"] == {"participation": 1, "win": 2}


def test_cumulative_series_starts_at_zero_and_sums_correctly():
    svc, repos = _build_service()
    repos["bet_repo"].get_player_bet_history.return_value = [
        {"bet_time": 1000, "profit": 10, "outcome": "won", "amount": 5, "leverage": 1, "match_id": 1},
        {"bet_time": 2000, "profit": -5, "outcome": "lost", "amount": 5, "leverage": 1, "match_id": 2},
        {"bet_time": 3000, "profit": 20, "outcome": "won", "amount": 10, "leverage": 1, "match_id": 3},
    ]
    series, _ = svc.get_balance_event_series(discord_id=1, guild_id=123)
    # Series: (1, 10), (2, 5), (3, 25)
    assert [cum for _, cum, _ in series] == [10, 5, 25]
    assert [idx for idx, _, _ in series] == [1, 2, 3]
