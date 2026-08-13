"""Persistent low-priority state and recorded-win progression."""

import sqlite3

import pytest

from repositories.match_repository import MatchRepository
from repositories.moderation_repository import ModerationRepository
from repositories.player_repository import PlayerRepository
from repositories.soft_avoid_repository import SoftAvoidRepository
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


def _low_priority_repo(db_path: str):
    from repositories.low_priority_repository import LowPriorityRepository

    return LowPriorityRepository(db_path)


def _register(
    player_repo: PlayerRepository,
    discord_id: int,
    guild_id: int,
    *,
    glicko_rd: float = 200.0,
) -> None:
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=glicko_rd,
        glicko_volatility=0.06,
        os_mu=35.0,
        os_sigma=8.0,
    )


def _record_low_priority_final_win(db_path: str):
    player_repo = PlayerRepository(db_path)
    match_repo = MatchRepository(db_path)
    low_priority_repo = _low_priority_repo(db_path)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        low_priority_repo=low_priority_repo,
    )
    player_ids = list(range(501, 511))
    for player_id in player_ids:
        _register(player_repo, player_id, TEST_GUILD_ID, glicko_rd=80.0)

    target_id = player_ids[0]
    low_priority_repo.set_low_priority(
        target_id,
        TEST_GUILD_ID,
        set_by=901,
        reason=None,
        wins_required=1,
    )
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    target_side = "radiant" if target_id in pending.radiant_team_ids else "dire"
    teammate_ids = (
        pending.radiant_team_ids if target_side == "radiant" else pending.dire_team_ids
    )
    peer_id = next(player_id for player_id in teammate_ids if player_id != target_id)
    result = match_service.record_match(target_side, guild_id=TEST_GUILD_ID)
    return {
        "match_service": match_service,
        "match_repo": match_repo,
        "player_repo": player_repo,
        "low_priority_repo": low_priority_repo,
        "target_id": target_id,
        "peer_id": peer_id,
        "match_id": result["match_id"],
    }


def _record_core_win(
    match_repo: MatchRepository,
    *,
    winner_id: int,
    loser_id: int,
    guild_id: int,
    pending_match_id: int | None = None,
) -> int:
    return match_repo.record_match_core_atomic(
        team1_ids=[winner_id],
        team2_ids=[loser_id],
        winning_team=1,
        guild_id=guild_id,
        dotabuff_match_id=None,
        lobby_type="shuffle",
        lobby_kind="open",
        balancing_rating_system="glicko",
        winning_ids=[winner_id],
        losing_ids=[loser_id],
        glicko_updates=[],
        openskill_updates=[],
        rating_history_rows=[],
        match_prediction={
            "radiant_rating": 1500.0,
            "dire_rating": 1500.0,
            "radiant_rd": 200.0,
            "dire_rd": 200.0,
            "expected_radiant_win_prob": 0.5,
        },
        last_match_date_iso="2026-07-27T00:00:00+00:00",
        first_calibration_ids=[],
        first_calibration_unix=0,
        effective_avoid_ids=[],
        effective_deal_ids=[],
        pending_match_id=pending_match_id,
    )


def test_schema_creates_low_priority_state(repo_db_path):
    with sqlite3.connect(repo_db_path) as conn:
        columns = {
            row[1]: row
            for row in conn.execute("PRAGMA table_info(low_priority_state)").fetchall()
        }

    assert set(columns) == {
        "discord_id",
        "guild_id",
        "wins_required",
        "wins_remaining",
        "start_pending_match_id",
        "active",
        "reason",
        "set_by",
        "removed_by",
        "removed_reason",
        "created_at",
        "updated_at",
    }
    assert columns["discord_id"][5] == 1
    assert columns["guild_id"][5] == 2


def test_set_clear_and_reset_persist_current_admin_state(repo_db_path):
    repo = _low_priority_repo(repo_db_path)

    first = repo.set_low_priority(
        101,
        TEST_GUILD_ID,
        set_by=901,
        reason="first reason",
    )
    assert first.active is True
    assert first.wins_remaining == 3
    assert first.reason == "first reason"
    assert first.set_by == 901

    repo.clear_low_priority(
        101,
        TEST_GUILD_ID,
        removed_by=902,
        reason="reviewed",
    )
    cleared = repo.get_state(101, TEST_GUILD_ID)
    assert cleared is not None
    assert cleared.active is False
    assert cleared.wins_remaining == 0
    assert cleared.removed_by == 902
    assert cleared.removed_reason == "reviewed"

    reset = repo.set_low_priority(101, TEST_GUILD_ID, set_by=903, reason=None)
    assert reset.active is True
    assert reset.wins_remaining == 3
    assert reset.set_by == 903
    assert reset.removed_by is None


def test_bulk_and_list_queries_are_active_only_and_guild_scoped(repo_db_path):
    repo = _low_priority_repo(repo_db_path)
    repo.set_low_priority(101, TEST_GUILD_ID, set_by=901, reason=None)
    repo.set_low_priority(102, TEST_GUILD_ID, set_by=901, reason=None)
    repo.set_low_priority(101, TEST_GUILD_ID_SECONDARY, set_by=902, reason=None)
    repo.clear_low_priority(102, TEST_GUILD_ID, removed_by=901, reason=None)

    assert repo.get_active_ids([101, 102, 999], TEST_GUILD_ID) == {101}
    assert repo.get_active_ids([], TEST_GUILD_ID) == set()
    assert [state.discord_id for state in repo.list_active(TEST_GUILD_ID)] == [101]
    assert repo.get_state(101, TEST_GUILD_ID_SECONDARY).active is True


def test_exactly_three_recorded_wins_release_only_the_winner(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    repo = _low_priority_repo(repo_db_path)
    winner_id = 101
    loser_id = 102
    for guild_id in (TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY):
        _register(player_repo, winner_id, guild_id)
        _register(player_repo, loser_id, guild_id)
    repo.set_low_priority(winner_id, TEST_GUILD_ID, set_by=901, reason=None)
    repo.set_low_priority(loser_id, TEST_GUILD_ID, set_by=901, reason=None)
    repo.set_low_priority(winner_id, TEST_GUILD_ID_SECONDARY, set_by=902, reason=None)

    for expected_remaining in (2, 1):
        _record_core_win(
            match_repo,
            winner_id=winner_id,
            loser_id=loser_id,
            guild_id=TEST_GUILD_ID,
        )
        state = repo.get_state(winner_id, TEST_GUILD_ID)
        assert state.active is True
        assert state.wins_remaining == expected_remaining

    _record_core_win(
        match_repo,
        winner_id=winner_id,
        loser_id=loser_id,
        guild_id=TEST_GUILD_ID,
    )

    released = repo.get_state(winner_id, TEST_GUILD_ID)
    assert released.active is False
    assert released.wins_remaining == 0
    assert repo.get_state(loser_id, TEST_GUILD_ID).wins_remaining == 3
    assert repo.get_state(winner_id, TEST_GUILD_ID_SECONDARY).wins_remaining == 3


def test_retrying_same_pending_match_does_not_double_count_win(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    repo = _low_priority_repo(repo_db_path)
    winner_id = 201
    loser_id = 202
    _register(player_repo, winner_id, TEST_GUILD_ID)
    _register(player_repo, loser_id, TEST_GUILD_ID)
    repo.set_low_priority(winner_id, TEST_GUILD_ID, set_by=901, reason=None)
    pending_match_id = match_repo.save_pending_match(
        TEST_GUILD_ID,
        {"radiant_team_ids": [winner_id], "dire_team_ids": [loser_id]},
    )

    first = _record_core_win(
        match_repo,
        winner_id=winner_id,
        loser_id=loser_id,
        guild_id=TEST_GUILD_ID,
        pending_match_id=pending_match_id,
    )
    second = _record_core_win(
        match_repo,
        winner_id=winner_id,
        loser_id=loser_id,
        guild_id=TEST_GUILD_ID,
        pending_match_id=pending_match_id,
    )

    assert second == first
    assert repo.get_state(winner_id, TEST_GUILD_ID).wins_remaining == 2


def test_assignment_ignores_a_match_that_was_already_pending(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    repo = _low_priority_repo(repo_db_path)
    winner_id = 301
    loser_id = 302
    _register(player_repo, winner_id, TEST_GUILD_ID)
    _register(player_repo, loser_id, TEST_GUILD_ID)
    existing_pending = match_repo.save_pending_match(
        TEST_GUILD_ID,
        {"radiant_team_ids": [winner_id], "dire_team_ids": [loser_id]},
    )
    repo.set_low_priority(
        winner_id,
        TEST_GUILD_ID,
        set_by=901,
        reason="future wins only",
        wins_required=2,
        start_pending_match_id=existing_pending,
    )

    _record_core_win(
        match_repo,
        winner_id=winner_id,
        loser_id=loser_id,
        guild_id=TEST_GUILD_ID,
        pending_match_id=existing_pending,
    )
    assert repo.get_state(winner_id, TEST_GUILD_ID).wins_remaining == 2

    future_pending = match_repo.save_pending_match(
        TEST_GUILD_ID,
        {"radiant_team_ids": [winner_id], "dire_team_ids": [loser_id]},
    )
    _record_core_win(
        match_repo,
        winner_id=winner_id,
        loser_id=loser_id,
        guild_id=TEST_GUILD_ID,
        pending_match_id=future_pending,
    )
    assert repo.get_state(winner_id, TEST_GUILD_ID).wins_remaining == 1


def test_automatic_completion_is_audited_once_with_match_id(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    repo = _low_priority_repo(repo_db_path)
    moderation_repo = ModerationRepository(repo_db_path)
    winner_id = 401
    loser_id = 402
    _register(player_repo, winner_id, TEST_GUILD_ID)
    _register(player_repo, loser_id, TEST_GUILD_ID)
    watermark = match_repo.save_pending_match(
        TEST_GUILD_ID,
        {"radiant_team_ids": [winner_id], "dire_team_ids": [loser_id]},
    )
    repo.set_low_priority(
        winner_id,
        TEST_GUILD_ID,
        set_by=901,
        reason="one future win",
        wins_required=1,
        start_pending_match_id=watermark,
    )
    pending_id = match_repo.save_pending_match(
        TEST_GUILD_ID,
        {"radiant_team_ids": [winner_id], "dire_team_ids": [loser_id]},
    )

    match_id = _record_core_win(
        match_repo,
        winner_id=winner_id,
        loser_id=loser_id,
        guild_id=TEST_GUILD_ID,
        pending_match_id=pending_id,
    )
    assert repo.get_state(winner_id, TEST_GUILD_ID).active is False
    events = moderation_repo.get_history(TEST_GUILD_ID, winner_id)
    assert len(events) == 1
    assert events[0].event_type.value == "lowprio_complete"
    assert events[0].match_id == match_id

    assert (
        _record_core_win(
            match_repo,
            winner_id=winner_id,
            loser_id=loser_id,
            guild_id=TEST_GUILD_ID,
            pending_match_id=pending_id,
        )
        == match_id
    )
    assert len(moderation_repo.get_history(TEST_GUILD_ID, winner_id)) == 1


def test_final_low_priority_win_boosts_both_rating_gains_before_release(repo_db_path):
    recorded = _record_low_priority_final_win(repo_db_path)
    player_repo = recorded["player_repo"]
    match_repo = recorded["match_repo"]
    target_id = recorded["target_id"]
    peer_id = recorded["peer_id"]

    target_rating = player_repo.get_glicko_rating(target_id, TEST_GUILD_ID)[0]
    peer_rating = player_repo.get_glicko_rating(peer_id, TEST_GUILD_ID)[0]
    assert target_rating - 1500.0 == pytest.approx((peer_rating - 1500.0) * 1.10)

    target_mu = player_repo.get_openskill_rating(target_id, TEST_GUILD_ID)[0]
    peer_mu = player_repo.get_openskill_rating(peer_id, TEST_GUILD_ID)[0]
    assert target_mu - 35.0 == pytest.approx((peer_mu - 35.0) * 1.10)

    state = recorded["low_priority_repo"].get_state(target_id, TEST_GUILD_ID)
    assert state.active is False
    history = {
        row["discord_id"]: row
        for row in match_repo.get_full_rating_history_for_match(recorded["match_id"])
    }
    assert history[target_id]["low_priority_gain_multiplier"] == pytest.approx(1.10)
    assert history[peer_id]["low_priority_gain_multiplier"] == pytest.approx(1.0)


def test_recording_rejects_low_priority_state_changed_after_rating_snapshot(
    repo_db_path,
    monkeypatch,
):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    low_priority_repo = _low_priority_repo(repo_db_path)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        low_priority_repo=low_priority_repo,
    )
    player_ids = list(range(601, 611))
    for player_id in player_ids:
        _register(player_repo, player_id, TEST_GUILD_ID, glicko_rd=80.0)

    target_id = player_ids[0]
    low_priority_repo.set_low_priority(
        target_id,
        TEST_GUILD_ID,
        set_by=901,
        reason="clear during recording",
        wins_required=1,
    )
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    target_side = "radiant" if target_id in pending.radiant_team_ids else "dire"

    get_active_ids = low_priority_repo.get_active_ids

    def get_then_clear(discord_ids, guild_id):
        active_ids = get_active_ids(discord_ids, guild_id)
        low_priority_repo.clear_low_priority(
            target_id,
            guild_id,
            removed_by=902,
            reason="concurrent admin clear",
        )
        return active_ids

    monkeypatch.setattr(low_priority_repo, "get_active_ids", get_then_clear)

    with pytest.raises(RuntimeError, match="Low-priority state changed"):
        match_service.record_match(target_side, guild_id=TEST_GUILD_ID)

    with sqlite3.connect(repo_db_path) as conn:
        assert conn.execute("SELECT COUNT(*) FROM matches").fetchone()[0] == 0
    assert match_service.get_last_shuffle(TEST_GUILD_ID) is not None

    result = match_service.record_match(target_side, guild_id=TEST_GUILD_ID)
    history = {
        row["discord_id"]: row
        for row in match_repo.get_full_rating_history_for_match(result["match_id"])
    }
    assert history[target_id]["low_priority_gain_multiplier"] == pytest.approx(1.0)


def test_openskill_replay_retains_recorded_low_priority_gain(repo_db_path):
    recorded = _record_low_priority_final_win(repo_db_path)
    match_service = recorded["match_service"]
    player_repo = recorded["player_repo"]
    target_id = recorded["target_id"]
    peer_id = recorded["peer_id"]

    replay = match_service.backfill_openskill_ratings(
        guild_id=TEST_GUILD_ID,
        reset_first=True,
    )

    assert replay["errors"] == []
    target_mu = player_repo.get_openskill_rating(target_id, TEST_GUILD_ID)[0]
    peer_mu = player_repo.get_openskill_rating(peer_id, TEST_GUILD_ID)[0]
    history = {
        row["discord_id"]: row
        for row in recorded["match_repo"].get_full_rating_history_for_match(
            recorded["match_id"]
        )
    }
    target_delta = target_mu - history[target_id]["os_mu_before"]
    peer_delta = peer_mu - history[peer_id]["os_mu_before"]
    assert target_delta == pytest.approx(peer_delta * 1.10)


def test_shuffle_display_uses_directional_low_priority_avoid_strength(
    repo_db_path, monkeypatch
):
    from config import SOFT_AVOID_PENALTY
    from domain.low_priority_constants import (
        LOW_PRIORITY_GOODNESS_PENALTY,
        LOW_PRIORITY_TEAM_GROUPING_BONUS,
    )
    from domain.models.team import Team
    from shuffler import BalancedShuffler

    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    low_priority_repo = _low_priority_repo(repo_db_path)
    soft_avoid_repo = SoftAvoidRepository(repo_db_path)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        low_priority_repo=low_priority_repo,
        soft_avoid_repo=soft_avoid_repo,
    )
    player_ids = list(range(601, 611))
    for player_id in player_ids:
        _register(player_repo, player_id, TEST_GUILD_ID)

    avoider_id, target_id = player_ids[:2]
    soft_avoid_repo.create_or_reactivate_avoid(
        TEST_GUILD_ID,
        avoider_id,
        target_id,
    )

    def fixed_teams(_shuffler, players, **_kwargs):
        roles = ["1", "2", "3", "4", "5"]
        return Team(players[:5], roles), Team(players[5:], roles)

    monkeypatch.setattr(BalancedShuffler, "shuffle", fixed_teams)

    baseline = match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    low_priority_repo.set_low_priority(
        target_id,
        TEST_GUILD_ID,
        set_by=901,
        reason=None,
    )
    target_only = match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    low_priority_repo.set_low_priority(
        avoider_id,
        TEST_GUILD_ID,
        set_by=901,
        reason=None,
    )
    both = match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

    assert target_only["goodness_score"] - baseline["goodness_score"] == pytest.approx(
        LOW_PRIORITY_GOODNESS_PENALTY + SOFT_AVOID_PENALTY
    )
    assert both["goodness_score"] - target_only["goodness_score"] == pytest.approx(
        LOW_PRIORITY_GOODNESS_PENALTY
        - SOFT_AVOID_PENALTY
        - LOW_PRIORITY_TEAM_GROUPING_BONUS
    )
