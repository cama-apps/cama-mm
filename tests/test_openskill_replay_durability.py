"""Durability and concurrency invariants for OpenSkill v3 replay."""

from __future__ import annotations

import time

import pytest

from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from repositories.recalibration_repository import RecalibrationRepository
from services.match_service import MatchService
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID


def _build(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=None,
    )
    return match_service, PlayerService(player_repo), player_repo, match_repo


def _seed_and_record(match_service, player_repo) -> tuple[list[int], int]:
    player_ids = list(range(92000, 92010))
    for player_id in player_ids:
        player_repo.add(
            discord_id=player_id,
            discord_username=f"Replay{player_id}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            preferred_roles=["1", "2", "3", "4", "5"],
            glicko_rating=1500.0,
            glicko_rd=120.0,
            glicko_volatility=0.06,
        )
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
    return player_ids, result["match_id"]


def test_admin_mu_and_sigma_events_survive_full_replay(repo_db_path):
    match_service, player_service, player_repo, match_repo = _build(repo_db_path)
    player_ids, _match_id = _seed_and_record(match_service, player_repo)
    target = player_ids[0]

    player_service.update_both_display_rating(
        target,
        TEST_GUILD_ID,
        1750.0,
        120.0,
        0.06,
        reason="test_override",
        actor_id=123,
    )
    player_service.bump_glicko_rds(
        TEST_GUILD_ID,
        50.0,
        actor_id=456,
    )
    before = player_repo.get_openskill_rating(target, TEST_GUILD_ID)

    summary = match_service.backfill_openskill_ratings(
        guild_id=TEST_GUILD_ID,
        reset_first=True,
    )

    assert summary["errors"] == []
    assert player_repo.get_openskill_rating(
        target,
        TEST_GUILD_ID,
    ) == pytest.approx(before)
    with match_repo.connection() as conn:
        events = conn.execute(
            """
            SELECT event_type, value, actor_id
            FROM openskill_rating_events
            WHERE guild_id = ? AND discord_id = ?
            ORDER BY event_id
            """,
            (TEST_GUILD_ID, target),
        ).fetchall()
    assert [row["event_type"] for row in events] == ["set_mu", "add_sigma"]
    assert events[0]["actor_id"] == 123
    assert events[1]["actor_id"] == 456


def test_recalibration_sigma_event_survives_full_replay(repo_db_path):
    match_service, _player_service, player_repo, _match_repo = _build(repo_db_path)
    player_ids, _match_id = _seed_and_record(match_service, player_repo)
    target = player_ids[0]
    mu_before, _sigma_before = player_repo.get_openskill_rating(
        target,
        TEST_GUILD_ID,
    )
    recalibration_repo = RecalibrationRepository(repo_db_path)
    recalibration_repo.execute_recalibration_atomic(
        discord_id=target,
        guild_id=TEST_GUILD_ID,
        now=int(time.time()),
        cooldown_seconds=3600,
        rating=1500.0,
        new_rd=350.0,
        new_volatility=0.06,
        new_os_sigma=7.5,
    )

    summary = match_service.backfill_openskill_ratings(
        guild_id=TEST_GUILD_ID,
        reset_first=True,
    )

    assert summary["errors"] == []
    assert player_repo.get_openskill_rating(
        target,
        TEST_GUILD_ID,
    ) == pytest.approx((mu_before, 7.5))


def test_stale_match_calculation_is_rejected_without_partial_commit(
    repo_db_path,
    monkeypatch,
):
    match_service, _player_service, player_repo, match_repo = _build(repo_db_path)
    player_ids = list(range(93000, 93010))
    for player_id in player_ids:
        player_repo.add(
            discord_id=player_id,
            discord_username=f"Race{player_id}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            preferred_roles=["1", "2", "3", "4", "5"],
            glicko_rating=1500.0,
            glicko_rd=120.0,
            glicko_volatility=0.06,
        )
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

    original_loader = (
        player_repo.get_match_rating_inputs_with_openskill_revision
    )

    def load_then_mutate(discord_ids, guild_id):
        rows, revision = original_loader(discord_ids, guild_id)
        player_repo.update_openskill_rating(
            player_ids[0],
            TEST_GUILD_ID,
            60.0,
            5.0,
        )
        return rows, revision

    monkeypatch.setattr(
        player_repo,
        "get_match_rating_inputs_with_openskill_revision",
        load_then_mutate,
    )

    with pytest.raises(RuntimeError, match="retry recording the match"):
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    assert match_repo.get_all_matches_chronological(TEST_GUILD_ID) == []
    assert player_repo.get_openskill_rating(
        player_ids[0],
        TEST_GUILD_ID,
    ) == pytest.approx((60.0, 5.0))


def test_complete_enrichment_marks_replay_pending_in_same_transaction(
    repo_db_path,
):
    match_service, _player_service, player_repo, match_repo = _build(repo_db_path)
    player_ids, match_id = _seed_and_record(match_service, player_repo)

    updated = match_repo.apply_enrichment_atomic(
        match_id=match_id,
        guild_id=TEST_GUILD_ID,
        valve_match_id=777,
        duration_seconds=2400,
        radiant_score=30,
        dire_score=20,
        game_mode=22,
        enrichment_data=None,
        enrichment_source="test",
        enrichment_confidence=1.0,
        participant_updates=[
            {
                "discord_id": player_id,
                "fantasy_points": 10.0 + index,
            }
            for index, player_id in enumerate(player_ids)
        ],
    )

    assert updated == 10
    pending = match_repo.get_pending_openskill_replay(TEST_GUILD_ID)
    assert pending is not None
    assert pending["reason"] == f"match_enrichment:{match_id}"


def test_runtime_replay_refreshes_predictions_and_fingerprint(repo_db_path):
    match_service, _player_service, player_repo, match_repo = _build(repo_db_path)
    _player_ids, match_id = _seed_and_record(match_service, player_repo)
    with match_repo.connection() as conn:
        conn.execute(
            """
            UPDATE match_predictions
            SET openskill_radiant_win_prob = 0.99,
                openskill_raw_radiant_win_prob = 0.99,
                openskill_algorithm_version = NULL,
                openskill_algorithm_fingerprint = NULL
            WHERE match_id = ?
            """,
            (match_id,),
        )

    summary = match_service.backfill_openskill_ratings(
        guild_id=TEST_GUILD_ID,
        reset_first=True,
    )

    assert summary["errors"] == []
    with match_repo.connection() as conn:
        prediction = conn.execute(
            """
            SELECT openskill_radiant_win_prob,
                   openskill_raw_radiant_win_prob,
                   openskill_algorithm_version,
                   openskill_algorithm_fingerprint
            FROM match_predictions
            WHERE match_id = ?
            """,
            (match_id,),
        ).fetchone()
    assert prediction["openskill_radiant_win_prob"] == pytest.approx(0.5)
    assert prediction["openskill_raw_radiant_win_prob"] == pytest.approx(0.5)
    assert prediction["openskill_algorithm_version"] == 3
    assert prediction["openskill_algorithm_fingerprint"]


def test_correction_replay_failure_has_idempotent_recovery(
    repo_db_path,
    monkeypatch,
):
    match_service, _player_service, player_repo, match_repo = _build(repo_db_path)
    _player_ids, match_id = _seed_and_record(match_service, player_repo)
    real_backfill = match_service.backfill_openskill_ratings

    def fail_replay(*_args, **_kwargs):
        raise RuntimeError("injected replay failure")

    monkeypatch.setattr(
        match_service,
        "backfill_openskill_ratings",
        fail_replay,
    )
    with pytest.raises(RuntimeError, match="injected replay failure"):
        match_service.correct_match_result(
            match_id,
            "dire",
            TEST_GUILD_ID,
            corrected_by=123,
        )

    assert match_repo.get_match(match_id, TEST_GUILD_ID)["winning_team"] == 2
    assert match_repo.get_pending_openskill_replay(TEST_GUILD_ID) is not None

    monkeypatch.setattr(
        match_service,
        "backfill_openskill_ratings",
        real_backfill,
    )
    recovered = match_service.correct_match_result(
        match_id,
        "dire",
        TEST_GUILD_ID,
        corrected_by=123,
    )

    assert recovered["replay_recovered"] is True
    assert match_repo.get_pending_openskill_replay(TEST_GUILD_ID) is None
