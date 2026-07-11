"""Direction tests for MatchService OpenSkill rating updates.

``update_openskill_ratings_for_match`` (Phase 2, fantasy-weighted) and
``backfill_openskill_ratings`` were only ever stubbed in the suite. These build
a real MatchService over real repositories, record a real match, and assert the
winners' OpenSkill mu moves UP and the losers' moves DOWN — and that the values
actually change. Assertions go through the PUBLIC service methods on purpose
(the task notes these keep their public signature even if the module is split).

OpenSkill direction is unambiguous here: with the per-game swing clamp
(``MAX_MU_SWING_PER_GAME``) and equal/near-equal weights, the Plackett-Luce
update raises the winning team's mu and lowers the losing team's.
"""

from __future__ import annotations

from openskill_rating_system import CamaOpenSkillSystem
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID

SEED_MU = 30.0
SEED_SIGMA = 8.0
SEED_MMR = 3000


def _build_service(repo_db_path) -> tuple[MatchService, PlayerRepository, MatchRepository]:
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=None,
    )
    return service, player_repo, match_repo


def _seed_players(player_repo: PlayerRepository, count: int = 10, *, guild_id=TEST_GUILD_ID) -> list[int]:
    ids = []
    for i in range(count):
        pid = 2000 + i
        player_repo.add(
            discord_id=pid,
            discord_username=f"OsPlayer{pid}",
            guild_id=guild_id,
            preferred_roles=["1", "2", "3", "4", "5"],
            initial_mmr=SEED_MMR,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.update_openskill_rating(pid, guild_id, SEED_MU, SEED_SIGMA)
        ids.append(pid)
    return ids


def _record_a_match(service, player_ids, *, guild_id=TEST_GUILD_ID):
    """Shuffle + record radiant win; return match_id."""
    service.shuffle_players(player_ids, guild_id=guild_id)
    result = service.record_match("radiant", guild_id=guild_id)
    return result["match_id"]


def _won_map(match_repo, match_id, *, guild_id=TEST_GUILD_ID) -> dict[int, bool]:
    parts = match_repo.get_match_participants(match_id, guild_id)
    return {p["discord_id"]: bool(p["won"]) for p in parts}


# --------------------------------------------------------------------------- #
# update_openskill_ratings_for_match (Phase 2, fantasy-weighted)
# --------------------------------------------------------------------------- #


def test_update_openskill_for_match_moves_winners_up_losers_down(repo_db_path):
    """Phase-2 recompute lifts winners and drops losers from the pre-match seed.

    record_match runs Phase-1 (equal weight) and stores the pre-match seed mu as
    os_mu_before in rating_history. Phase 2 starts from that seed, so we compare
    the post-Phase-2 player mu against SEED_MU directly.
    """
    service, player_repo, match_repo = _build_service(repo_db_path)
    player_ids = _seed_players(player_repo)
    match_id = _record_a_match(service, player_ids)

    # Give every participant identical fantasy points so direction is driven by
    # win/loss, not by FP differences.
    with match_repo.connection() as conn:
        conn.execute(
            "UPDATE match_participants SET fantasy_points = 15.0 WHERE match_id = ?",
            (match_id,),
        )

    won_by = _won_map(match_repo, match_id)
    assert sum(won_by.values()) == 5  # exactly one winning team

    res = service.update_openskill_ratings_for_match(match_id, guild_id=TEST_GUILD_ID)
    assert res["success"] is True
    assert res["players_updated"] == 10

    final = player_repo.get_openskill_ratings_bulk(player_ids, TEST_GUILD_ID)
    for pid in player_ids:
        mu = final[pid][0]
        if won_by[pid]:
            assert mu > SEED_MU, f"winner {pid} mu {mu} should exceed seed {SEED_MU}"
        else:
            assert mu < SEED_MU, f"loser {pid} mu {mu} should fall below seed {SEED_MU}"


def test_update_openskill_for_match_no_fantasy_data_skips(repo_db_path):
    """With no fantasy points on any participant, Phase 2 is a documented no-op.

    It must report success with everyone skipped and leave player mu untouched.
    """
    service, player_repo, _ = _build_service(repo_db_path)
    player_ids = _seed_players(player_repo)
    match_id = _record_a_match(service, player_ids)

    # Phase-1 already moved ratings; capture that as the no-op baseline.
    pre = player_repo.get_openskill_ratings_bulk(player_ids, TEST_GUILD_ID)

    res = service.update_openskill_ratings_for_match(match_id, guild_id=TEST_GUILD_ID)
    assert res["success"] is True
    assert res["players_updated"] == 0
    assert res["players_skipped"] == 10

    post = player_repo.get_openskill_ratings_bulk(player_ids, TEST_GUILD_ID)
    assert post == pre


def test_phase2_unrated_participant_records_default_baseline_not_none(repo_db_path):
    """Phase-2 overwrites rating_history.os_mu_before. When a participant is
    absent from the Phase-1 baseline (its before-columns are NULL, so the
    baseline query skips it) AND has no current OS rating, the engine recomputes
    from its default mu/sigma. The persisted 'before' must therefore be that
    default — not None — or the history row disagrees with the math that
    produced 'after'."""
    service, player_repo, match_repo = _build_service(repo_db_path)
    player_ids = _seed_players(player_repo)
    match_id = _record_a_match(service, player_ids)
    unrated = player_ids[0]
    with match_repo.connection() as conn:
        conn.execute(
            "UPDATE match_participants SET fantasy_points = 15.0 WHERE match_id = ?",
            (match_id,),
        )
        # Drop this player from the baseline (NULL before-cols) but keep the row
        # so Phase-2's UPDATE still targets it, and strip the current OS rating
        # so the bulk fallback yields (None, None).
        conn.execute(
            "UPDATE rating_history SET os_mu_before = NULL, os_sigma_before = NULL "
            "WHERE match_id = ? AND discord_id = ?",
            (match_id, unrated),
        )
        conn.execute(
            "UPDATE players SET os_mu = NULL, os_sigma = NULL "
            "WHERE discord_id = ? AND guild_id = ?",
            (unrated, TEST_GUILD_ID),
        )

    res = service.update_openskill_ratings_for_match(match_id, guild_id=TEST_GUILD_ID)
    assert res["success"] is True

    with match_repo.connection() as conn:
        row = conn.execute(
            "SELECT os_mu_before, os_sigma_before FROM rating_history "
            "WHERE match_id = ? AND discord_id = ?",
            (match_id, unrated),
        ).fetchone()
    assert row["os_mu_before"] == CamaOpenSkillSystem.DEFAULT_MU
    assert row["os_sigma_before"] == CamaOpenSkillSystem.DEFAULT_SIGMA


def test_update_openskill_for_match_missing_match_errors(repo_db_path):
    """A non-existent match id returns a failure dict, not an exception."""
    service, _player_repo, _match_repo = _build_service(repo_db_path)
    res = service.update_openskill_ratings_for_match(999999, guild_id=TEST_GUILD_ID)
    assert res["success"] is False
    assert "not found" in res["error"]


# --------------------------------------------------------------------------- #
# backfill_openskill_ratings
# --------------------------------------------------------------------------- #


def test_backfill_moves_winners_up_losers_down_from_reseed(repo_db_path):
    """Backfill with reset_first reseeds from initial_mmr, then replays the match.

    All players share SEED_MMR, so the reseeded baseline mu is identical
    (mmr_to_os_mu(SEED_MMR)). After replaying one match (no fantasy -> equal
    weight), winners sit above that baseline and losers below it, and the two
    groups are cleanly separated.

    Runs on guild 0 (guild_id=None): reseed and per-match replay both target
    guild 0, so the winner/loser separation is unambiguous. See
    ``test_backfill_routes_per_match_writes_to_correct_guild`` for the non-zero
    guild routing guarantee.
    """
    guild = None  # normalizes to 0
    service, player_repo, match_repo = _build_service(repo_db_path)
    player_ids = _seed_players(player_repo, guild_id=0)
    match_id = _record_a_match(service, player_ids, guild_id=guild)
    won_by = _won_map(match_repo, match_id, guild_id=0)

    baseline_mu = CamaOpenSkillSystem().mmr_to_os_mu(SEED_MMR)

    summary = service.backfill_openskill_ratings(guild_id=guild, reset_first=True)
    assert summary["matches_processed"] == 1
    assert summary["matches_equal_weight"] == 1
    assert summary["matches_with_fantasy"] == 0
    assert summary["errors"] == []

    final = player_repo.get_openskill_ratings_bulk(player_ids, 0)
    winners = [final[pid][0] for pid in player_ids if won_by[pid]]
    losers = [final[pid][0] for pid in player_ids if not won_by[pid]]

    assert all(mu > baseline_mu for mu in winners), winners
    assert all(mu < baseline_mu for mu in losers), losers
    # Every winner outranks every loser after the replay.
    assert min(winners) > max(losers)


def test_backfill_routes_per_match_writes_to_correct_guild(repo_db_path):
    """Regression: backfill on a non-zero guild writes the replayed ratings to
    THAT guild, not guild 0.

    ``get_all_matches_chronological`` previously omitted ``guild_id`` from its
    rows, so ``backfill_openskill_ratings`` read ``match.get("guild_id")`` as
    None and routed every per-match update to guild 0 — leaving the requested
    guild pinned at its reseed baseline. The query now surfaces ``guild_id`` so
    the replay lands on the match's own guild.
    """
    guild = TEST_GUILD_ID  # non-zero
    service, player_repo, match_repo = _build_service(repo_db_path)
    player_ids = _seed_players(player_repo, guild_id=guild)
    match_id = _record_a_match(service, player_ids, guild_id=guild)
    won_by = _won_map(match_repo, match_id, guild_id=guild)

    baseline_mu = CamaOpenSkillSystem().mmr_to_os_mu(SEED_MMR)
    before_g0 = player_repo.get_openskill_ratings_bulk(player_ids, 0)

    summary = service.backfill_openskill_ratings(guild_id=guild, reset_first=True)
    assert summary["matches_processed"] == 1
    assert summary["errors"] == []

    # The replay must move ratings ON THE REQUESTED GUILD. Before the fix these
    # stayed at baseline because the per-match writes leaked to guild 0.
    final = player_repo.get_openskill_ratings_bulk(player_ids, guild)
    winners = [final[pid][0] for pid in player_ids if won_by[pid]]
    losers = [final[pid][0] for pid in player_ids if not won_by[pid]]
    assert all(mu > baseline_mu for mu in winners), winners
    assert all(mu < baseline_mu for mu in losers), losers

    # ...and nothing should have leaked onto guild 0.
    after_g0 = player_repo.get_openskill_ratings_bulk(player_ids, 0)
    assert after_g0 == before_g0, "backfill on a non-zero guild must not write to guild 0"


def test_backfill_no_matches_reports_empty(repo_db_path):
    """Backfill on a guild with no matches returns the documented empty result."""
    service, player_repo, _match_repo = _build_service(repo_db_path)
    _seed_players(player_repo, guild_id=0)  # players exist, but no matches recorded
    summary = service.backfill_openskill_ratings(guild_id=None, reset_first=False)
    assert summary["matches_processed"] == 0
    assert summary["players_updated"] == 0
    assert summary["errors"] == ["No matches found"]


def test_openskill_prediction_uses_requested_guild_and_shared_probability_model(repo_db_path):
    """Prediction lookup is guild-scoped and uses the same model as shuffle preview."""
    service, player_repo, _match_repo = _build_service(repo_db_path)
    team1 = [3100, 3101, 3102, 3103, 3104]
    team2 = [3200, 3201, 3202, 3203, 3204]
    all_ids = team1 + team2

    target_guild = TEST_GUILD_ID
    other_guild = 0
    for pid in all_ids:
        for guild in (target_guild, other_guild):
            player_repo.add(
                discord_id=pid,
                discord_username=f"OsPlayer{pid}-{guild}",
                guild_id=guild,
                preferred_roles=["1", "2", "3", "4", "5"],
                initial_mmr=SEED_MMR,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

    target_updates = [(pid, 60.0, 4.0) for pid in team1] + [(pid, 35.0, 4.0) for pid in team2]
    other_updates = [(pid, 35.0, 4.0) for pid in team1] + [(pid, 60.0, 4.0) for pid in team2]
    player_repo.update_openskill_ratings_bulk(target_updates, target_guild)
    player_repo.update_openskill_ratings_bulk(other_updates, other_guild)

    target_prediction = service.get_openskill_predictions_for_match(
        team1, team2, guild_id=target_guild
    )
    other_prediction = service.get_openskill_predictions_for_match(
        team1, team2, guild_id=other_guild
    )

    expected_raw = service.openskill_system.os_predict_win_probability(
        [(60.0, 4.0)] * 5,
        [(35.0, 4.0)] * 5,
    )
    expected_calibrated = service.openskill_system.calibrate_win_probability(expected_raw)
    assert target_prediction["raw_team1_win_prob"] == expected_raw
    assert target_prediction["team1_win_prob"] == expected_calibrated
    assert target_prediction["team1_win_prob"] < target_prediction["raw_team1_win_prob"]
    assert target_prediction["team1_win_prob"] > 0.5
    assert other_prediction["team1_win_prob"] < 0.5
