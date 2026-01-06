import pytest

from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService


def test_record_match_stores_predictions_and_history(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)

    player_ids = list(range(9101, 9111))
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            initial_mmr=4000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

    match_service.shuffle_players(player_ids, guild_id=1)
    result = match_service.record_match("radiant", guild_id=1)

    predictions = match_repo.get_recent_match_predictions(limit=1)
    assert len(predictions) == 1
    assert predictions[0]["match_id"] == result["match_id"]
    assert predictions[0]["expected_radiant_win_prob"] == pytest.approx(0.5, rel=1e-6)

    history = match_repo.get_recent_rating_history(limit=20)
    assert len(history) == 10
    assert {entry["match_id"] for entry in history} == {result["match_id"]}
    for entry in history:
        assert entry["rating_before"] is not None
        assert entry["rd_before"] is not None
        assert entry["expected_team_win_prob"] == pytest.approx(0.5, rel=1e-6)
        assert entry["team_number"] in (1, 2)
        assert entry["won"] in (0, 1, True, False)
