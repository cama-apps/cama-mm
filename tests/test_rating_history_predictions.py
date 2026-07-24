import sqlite3

import pytest

from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


def test_recent_rating_history_queries_use_chronology_indexes(repo_db_path):
    with sqlite3.connect(repo_db_path) as conn:
        guild_plan = conn.execute(
            """
            EXPLAIN QUERY PLAN
            SELECT * FROM rating_history
            WHERE guild_id = ?
            ORDER BY timestamp DESC
            LIMIT ?
            """,
            (TEST_GUILD_ID, 2000),
        ).fetchall()
        player_plan = conn.execute(
            """
            EXPLAIN QUERY PLAN
            SELECT * FROM rating_history
            WHERE discord_id = ? AND guild_id = ?
            ORDER BY timestamp DESC
            LIMIT ?
            """,
            (1, TEST_GUILD_ID, 50),
        ).fetchall()

    assert any("idx_rating_history_guild_time" in row[3] for row in guild_plan)
    assert any(
        "idx_rating_history_guild_player_time" in row[3] for row in player_plan
    )
    assert not any("TEMP B-TREE FOR ORDER BY" in row[3] for row in guild_plan)
    assert not any("TEMP B-TREE FOR ORDER BY" in row[3] for row in player_plan)


def test_record_match_stores_predictions_and_history(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)

    player_ids = list(range(9101, 9111))
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=4000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    predictions = match_repo.get_recent_match_predictions(guild_id=TEST_GUILD_ID, limit=1)
    assert len(predictions) == 1
    assert predictions[0]["match_id"] == result["match_id"]
    assert predictions[0]["expected_radiant_win_prob"] == pytest.approx(0.5, rel=1e-6)

    history = match_repo.get_recent_rating_history(guild_id=TEST_GUILD_ID, limit=20)
    assert len(history) == 10
    assert {entry["match_id"] for entry in history} == {result["match_id"]}
    for entry in history:
        assert entry["rating_before"] is not None
        assert entry["rd_before"] is not None
        assert entry["expected_team_win_prob"] == pytest.approx(0.5, rel=1e-6)
        assert entry["team_number"] in (1, 2)
        assert entry["won"] in (0, 1, True, False)


@pytest.mark.parametrize(
    ("special_rating", "special_mmr"),
    [(0.0, 500), (None, 8000)],
)
def test_shuffle_prediction_preserves_zero_and_uses_discounted_missing_seed(
    repo_db_path, special_rating, special_mmr
):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
    )

    player_ids = list(range(9201, 9211))
    for index, pid in enumerate(player_ids):
        is_special = index == 0
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=special_mmr if is_special else 6000,
            glicko_rating=special_rating if is_special else 1500.0,
            glicko_rd=350.0 if is_special else 100.0,
            glicko_volatility=0.06,
        )

    result = match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

    def aggregate(team):
        glicko_players = []
        for player in team.players:
            if player.glicko_rating is None:
                glicko_players.append(
                    match_service.rating_system.create_player_from_mmr(player.mmr)
                )
            else:
                glicko_players.append(
                    match_service.rating_system.create_player_from_rating(
                        player.glicko_rating,
                        player.glicko_rd,
                        player.glicko_volatility,
                    )
                )
        return match_service.rating_system.aggregate_team_stats(glicko_players)

    radiant_rating, radiant_rd, _ = aggregate(result["radiant_team"])
    dire_rating, dire_rd, _ = aggregate(result["dire_team"])
    expected = match_service.rating_system.predict_win_probability(
        radiant_rating,
        radiant_rd,
        dire_rating,
        dire_rd,
    )

    assert result["glicko_radiant_win_prob"] == pytest.approx(expected)
