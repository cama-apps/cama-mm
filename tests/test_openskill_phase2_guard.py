"""
Guards around post-enrichment (Phase 2) OpenSkill updates and enrichment.

1. apply_openskill_phase2_atomic must only rewrite players.os_mu/os_sigma when
   the enriched match is the player's LATEST rated match. Phase 2 recomputes
   from the enriched match's own Phase 1 baseline, so enriching an OLD match
   (fantasy refill, manual /enrich of history) used to rewind players' live
   ratings to stale post-that-match values. rating_history for the match must
   still always be updated.

2. enrich_match with skip_validation and ZERO matched participants must fail
   without committing — otherwise the match is marked enriched with no stats
   and disappears from refill lists forever.
"""

from unittest.mock import Mock

import pytest

from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_enrichment_service import MatchEnrichmentService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def phase2_services(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    match_service = MatchService(
        player_repo=player_repo, match_repo=match_repo, use_glicko=True
    )
    return {
        "player_repo": player_repo,
        "match_repo": match_repo,
        "match_service": match_service,
    }


def _create_players(player_repo, start_id, count=10):
    player_ids = list(range(start_id, start_id + count))
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
        )
    return player_ids


def _record_match(match_service, player_ids):
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
    return result["match_id"]


def _set_fantasy_points(match_repo, match_id):
    """Give every participant a distinct fantasy score so Phase 2 weights
    genuinely differ from the equal-weight Phase 1 values."""
    participants = match_repo.get_match_participants(match_id, TEST_GUILD_ID)
    updates = [
        {"discord_id": p["discord_id"], "fantasy_points": 5.0 + 3.0 * i}
        for i, p in enumerate(participants)
    ]
    match_repo.update_participant_stats_bulk(match_id, updates)


class TestPhase2LatestMatchGuard:
    def test_enriching_latest_match_updates_players(self, phase2_services):
        """Phase 2 on the player's latest match updates the live os ratings,
        and they land exactly on the rating_history after-values."""
        match_service = phase2_services["match_service"]
        match_repo = phase2_services["match_repo"]
        player_repo = phase2_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=21000)
        match_id = _record_match(match_service, player_ids)
        _set_fantasy_points(match_repo, match_id)

        phase1 = player_repo.get_openskill_ratings_bulk(player_ids, TEST_GUILD_ID)

        result = match_service.update_openskill_ratings_for_match(
            match_id, guild_id=TEST_GUILD_ID
        )
        assert result["success"] is True
        assert result["players_updated"] == 10

        history = {
            e["discord_id"]: e
            for e in match_repo.get_full_rating_history_for_match(match_id)
        }
        current = player_repo.get_openskill_ratings_bulk(player_ids, TEST_GUILD_ID)
        for pid in player_ids:
            mu, sigma = current[pid]
            assert mu == pytest.approx(history[pid]["os_mu_after"])
            assert sigma == pytest.approx(history[pid]["os_sigma_after"])
            assert history[pid]["fantasy_weight"] is not None
        # The skewed fantasy weights must move at least some players away
        # from their Phase 1 values, or this test proves nothing.
        assert any(
            current[pid][0] != pytest.approx(phase1[pid][0]) for pid in player_ids
        )

    def test_enriching_old_match_leaves_players_untouched(self, phase2_services):
        """Phase 2 on an OLD match (a newer rated match exists) must update
        only that match's rating_history — players.os_* stays at the live
        post-match-2 values instead of rewinding to stale post-match-1 ones."""
        match_service = phase2_services["match_service"]
        match_repo = phase2_services["match_repo"]
        player_repo = phase2_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=22000)
        match1_id = _record_match(match_service, player_ids)
        match2_id = _record_match(match_service, player_ids)
        assert match2_id > match1_id
        _set_fantasy_points(match_repo, match1_id)

        live = player_repo.get_openskill_ratings_bulk(player_ids, TEST_GUILD_ID)

        result = match_service.update_openskill_ratings_for_match(
            match1_id, guild_id=TEST_GUILD_ID
        )
        assert result["success"] is True
        assert result["players_updated"] == 0, (
            "enriching an old match must not rewrite live player ratings"
        )

        after = player_repo.get_openskill_ratings_bulk(player_ids, TEST_GUILD_ID)
        assert after == live, "players.os_* must be byte-identical after old-match Phase 2"

        # The old match's history rows still got the fantasy-weighted rewrite.
        history = {
            e["discord_id"]: e
            for e in match_repo.get_full_rating_history_for_match(match1_id)
        }
        for pid in player_ids:
            assert history[pid]["fantasy_weight"] is not None


class TestEnrichZeroMatchedParticipants:
    """enrich_match with skip_validation must not commit when nothing matched."""

    def _service(self, participants, od_players):
        match_repo = Mock()
        player_repo = Mock()
        opendota_api = Mock()
        match_repo.get_match.return_value = {"match_id": 1, "winning_team": 1}
        match_repo.get_match_participants.return_value = participants
        player_repo.get_steam_ids_bulk.return_value = {
            p["discord_id"]: p.get("_steam_ids", []) for p in participants
        }
        opendota_api.get_match_details.return_value = {
            "match_id": 999,
            "duration": 2400,
            "radiant_win": True,
            "radiant_score": 30,
            "dire_score": 20,
            "game_mode": 2,
            "players": od_players,
        }
        service = MatchEnrichmentService(match_repo, player_repo, opendota_api)
        return service, match_repo

    def test_zero_matched_participants_is_a_failure_and_writes_nothing(self):
        participants = [
            {"discord_id": 100, "side": "radiant", "_steam_ids": [111]},
            {"discord_id": 101, "side": "dire", "_steam_ids": []},
        ]
        # OpenDota players have account_ids that match nobody.
        od_players = [{"account_id": 555, "player_slot": 0, "kills": 3}]

        service, match_repo = self._service(participants, od_players)
        result = service.enrich_match(
            1, 999, skip_validation=True, guild_id=TEST_GUILD_ID
        )

        assert result["success"] is False
        assert result["players_enriched"] == 0
        match_repo.apply_enrichment_atomic.assert_not_called()

    def test_one_matched_participant_still_enriches(self):
        """Positive control: the zero-guard must not block a real (if partial)
        enrichment under skip_validation."""
        participants = [
            {"discord_id": 100, "side": "radiant", "_steam_ids": [111]},
            {"discord_id": 101, "side": "dire", "_steam_ids": []},
        ]
        od_players = [
            {"account_id": 111, "player_slot": 0, "kills": 3, "deaths": 2, "assists": 4}
        ]

        service, match_repo = self._service(participants, od_players)
        result = service.enrich_match(
            1, 999, skip_validation=True, guild_id=TEST_GUILD_ID
        )

        assert result["success"] is True
        assert result["players_enriched"] == 1
        match_repo.apply_enrichment_atomic.assert_called_once()
