"""Unit tests for ``BalanceHistoryService`` — merges eight event sources into one series."""

from __future__ import annotations

import json
from unittest.mock import MagicMock

from services.balance_history_service import (
    SOURCE_BETS,
    SOURCE_BONUS,
    SOURCE_DIG,
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
        "dig_repo": MagicMock(),
    }
    # Every repo returns an empty list by default so tests only populate what they need.
    repos["bet_repo"].get_player_bet_history.return_value = []
    repos["match_repo"].get_player_bonus_events.return_value = []
    repos["player_repo"].get_wheel_spin_history.return_value = []
    repos["player_repo"].get_double_or_nothing_history.return_value = []
    repos["prediction_repo"].get_player_prediction_history.return_value = []
    repos["disburse_repo"].get_recipient_history.return_value = []
    repos["tip_repo"].get_all_tips_for_user.return_value = []
    repos["dig_repo"].get_player_jc_events.return_value = []
    repos.update(overrides)
    return BalanceHistoryService(**repos), repos


def _dig_row(*, actor_id=1, target_id=None, action_type="dig", detail=None, created_at=1000, jc_delta=0):
    return {
        "actor_id": actor_id,
        "target_id": target_id,
        "action_type": action_type,
        "detail": json.dumps(detail) if detail else None,
        "created_at": created_at,
        "jc_delta": jc_delta,
    }


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


def test_dig_action_earnings_credit_actor():
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(action_type="dig", detail={"jc": 3, "advance": 2}, created_at=100),
        _dig_row(action_type="dig", detail={"jc": 5, "advance": 4}, created_at=200),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [3, 5]
    assert totals == {SOURCE_DIG: 8}


def test_dig_cave_in_logs_are_silently_skipped():
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(action_type="dig", detail={"cave_in": True, "block_loss": 3}, created_at=100),
        _dig_row(action_type="dig", detail={"jc": 2}, created_at=200),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    # Cave-in log has no "jc" field → delta=0 → skipped.
    assert [info["delta"] for _, _, info in series] == [2]
    assert totals == {SOURCE_DIG: 2}


def test_dig_sabotage_trap_debits_actor_and_credits_target():
    svc, repos = _build_service()
    # From actor's perspective: trap_triggered, lost jc_lost=10 → delta = -10.
    # From target's perspective: same row; target gains jc_lost // 2 = 5.
    trap_detail = {"target_id": 2, "trap_triggered": True, "jc_lost": 10, "blocks_lost": 3}

    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(actor_id=1, target_id=2, action_type="sabotage", detail=trap_detail, created_at=100),
    ]
    series_actor, _ = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series_actor] == [-10]

    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(actor_id=1, target_id=2, action_type="sabotage", detail=trap_detail, created_at=100),
    ]
    series_target, _ = svc.get_balance_event_series(discord_id=2, guild_id=123)
    assert [info["delta"] for _, _, info in series_target] == [5]


def test_dig_sabotage_no_trap_debits_cost_and_target_untouched():
    svc, repos = _build_service()
    detail = {"target_id": 2, "damage": 5, "cost": 7, "trap_triggered": False}
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(actor_id=1, target_id=2, action_type="sabotage", detail=detail, created_at=100),
    ]
    # Actor loses the cost; target takes block damage only (no JC change).
    series_actor, _ = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series_actor] == [-7]

    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(actor_id=1, target_id=2, action_type="sabotage", detail=detail, created_at=100),
    ]
    series_target, _ = svc.get_balance_event_series(discord_id=2, guild_id=123)
    assert series_target == []


def test_dig_boss_fight_wagered_win_returns_net_not_gross():
    """Wagered boss wins log the gross payout as jc_delta but credit gross - wager."""
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        # Wager 5, gross jc_delta 15 → net credit = 10.
        _dig_row(action_type="boss_fight",
                 detail={"boundary": 1, "won": True, "wager": 5, "jc_delta": 15}, created_at=100),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [10]
    assert totals == {SOURCE_DIG: 10}


def test_dig_boss_fight_unwagered_win_uses_full_jc_delta():
    """Non-wagered boss wins credit the full jc_delta (a random 5-15 roll)."""
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(action_type="boss_fight",
                 detail={"boundary": 1, "won": True, "wager": 0, "jc_delta": 12}, created_at=100),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [12]
    assert totals == {SOURCE_DIG: 12}


def test_dig_boss_fight_phase1_win_with_no_jc_delta_is_zero():
    """Phase-1 transitions log won=True with no jc_delta key; no balance change."""
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(action_type="boss_fight",
                 detail={"boundary": 1, "won": True, "phase": 1, "wager": 10}, created_at=100),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert series == []
    assert totals == {}


def test_dig_boss_fight_loss_debits_wager():
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(action_type="boss_fight",
                 detail={"boundary": 1, "won": False, "wager": 8}, created_at=100),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [-8]
    assert totals == {SOURCE_DIG: -8}


def test_dig_event_handles_jc_delta_cruel_echoes_and_jc_keys():
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(action_type="event",
                 detail={"event_id": "x", "choice": "A", "jc_delta": 4, "depth_delta": 1},
                 created_at=100),
        _dig_row(action_type="event",
                 detail={"event_id": "y", "choice": "B", "cruel_echoes": True},
                 created_at=200),
        _dig_row(action_type="event",
                 detail={"event_id": "z", "choice": "C", "succeeded": True, "jc": 6, "advance": 2},
                 created_at=300),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [4, -1, 6]
    assert totals == {SOURCE_DIG: 9}


def test_dig_help_always_credits_one_jc():
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(actor_id=1, target_id=2, action_type="help",
                 detail={"target_id": 2, "advance": 1}, created_at=100),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [1]
    assert totals == {SOURCE_DIG: 1}


def test_dig_abandon_refund_and_retreat_loss():
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(action_type="abandon", detail={"depth": 5, "refund": 3}, created_at=100),
        _dig_row(action_type="boss_retreat", detail={"boundary": 1, "loss": 2}, created_at=200),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [3, -2]
    assert totals == {SOURCE_DIG: 1}


def test_dig_prestige_and_unknown_action_types_skipped():
    svc, repos = _build_service()
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(action_type="prestige", detail={"level": 2, "perk": "fast_dig"}, created_at=100),
        _dig_row(action_type="nonsense_made_up", detail={"jc": 100}, created_at=200),
    ]
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert series == []
    assert totals == {}


def test_dig_jc_delta_column_overrides_json_when_populated():
    svc, repos = _build_service()
    # Future-proofing: if a log writes jc_delta into the column, trust it.
    repos["dig_repo"].get_player_jc_events.return_value = [
        _dig_row(action_type="dig", detail={"jc": 2}, jc_delta=999, created_at=100),
    ]
    series, _ = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert [info["delta"] for _, _, info in series] == [999]


def test_dig_missing_repo_produces_no_dig_events():
    """If dig_repo is not provided, the service silently skips the dig source."""
    svc = BalanceHistoryService(
        bet_repo=MagicMock(get_player_bet_history=MagicMock(return_value=[])),
        match_repo=MagicMock(get_player_bonus_events=MagicMock(return_value=[])),
        player_repo=MagicMock(
            get_wheel_spin_history=MagicMock(return_value=[]),
            get_double_or_nothing_history=MagicMock(return_value=[]),
        ),
        prediction_repo=MagicMock(get_player_prediction_history=MagicMock(return_value=[])),
        disburse_repo=MagicMock(get_recipient_history=MagicMock(return_value=[])),
        tip_repo=MagicMock(get_all_tips_for_user=MagicMock(return_value=[])),
        dig_repo=None,
    )
    series, totals = svc.get_balance_event_series(discord_id=1, guild_id=123)
    assert series == []
    assert totals == {}


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
