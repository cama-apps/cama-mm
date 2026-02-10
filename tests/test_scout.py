"""
Tests for the /scout command functionality.
"""

import json

import pytest

from tests.conftest import TEST_GUILD_ID


class TestScoutRepositoryMethods:
    """Tests for scout-related repository methods."""

    def test_get_player_hero_stats_for_scout_empty_list(self, match_repository):
        """Should return empty dict for empty player list."""
        result = match_repository.get_player_hero_stats_for_scout([])
        assert result == {}

    def test_get_player_hero_stats_for_scout_no_data(self, match_repository):
        """Should return empty dict when players have no match data."""
        result = match_repository.get_player_hero_stats_for_scout([999, 998], TEST_GUILD_ID)
        assert result == {}

    def test_get_player_hero_stats_for_scout_with_data(
        self, match_repository, player_repository
    ):
        """Should return hero stats organized by player."""
        # Register players
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        # Record a match
        match_id = match_repository.record_match(
            team1_ids=[100],
            team2_ids=[200],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )

        # Add participant data with hero info
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000, lane_role=1,
        )
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=200, hero_id=2,
            kills=5, deaths=8, assists=3, gpm=400, xpm=400,
            hero_damage=15000, tower_damage=2000, last_hits=100,
            denies=5, net_worth=12000, lane_role=2,
        )

        # Get scout stats
        result = match_repository.get_player_hero_stats_for_scout([100, 200], TEST_GUILD_ID)

        assert 100 in result
        assert 200 in result
        assert len(result[100]) == 1
        assert len(result[200]) == 1
        assert result[100][0]["hero_id"] == 1
        assert result[100][0]["games"] == 1
        assert result[100][0]["wins"] == 1
        assert result[100][0]["losses"] == 0
        assert result[100][0]["primary_role"] == 1
        assert result[200][0]["hero_id"] == 2
        assert result[200][0]["wins"] == 0
        assert result[200][0]["losses"] == 1

    def test_get_player_hero_stats_multiple_games(
        self, match_repository, player_repository
    ):
        """Should aggregate stats across multiple games."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        # Record multiple matches with same hero
        for i in range(3):
            match_id = match_repository.record_match(
                team1_ids=[100],
                team2_ids=[200],
                winning_team=1 if i < 2 else 2,  # 2 wins, 1 loss
                guild_id=TEST_GUILD_ID,
            )
            match_repository.update_participant_stats(
                match_id=match_id, discord_id=100, hero_id=1,
                kills=10, deaths=2, assists=5, gpm=600, xpm=500,
                hero_damage=20000, tower_damage=5000, last_hits=200,
                denies=10, net_worth=20000, lane_role=1,
            )
            match_repository.update_participant_stats(
                match_id=match_id, discord_id=200, hero_id=2,
                kills=5, deaths=8, assists=3, gpm=400, xpm=400,
                hero_damage=15000, tower_damage=2000, last_hits=100,
                denies=5, net_worth=12000, lane_role=2,
            )

        result = match_repository.get_player_hero_stats_for_scout([100], TEST_GUILD_ID)

        assert 100 in result
        hero_stat = result[100][0]
        assert hero_stat["hero_id"] == 1
        assert hero_stat["games"] == 3
        assert hero_stat["wins"] == 2
        assert hero_stat["losses"] == 1

    def test_get_bans_for_players_empty_list(self, match_repository):
        """Should return empty dict for empty player list."""
        result = match_repository.get_bans_for_players([])
        assert result == {}

    def test_get_bans_for_players_no_enrichment_data(
        self, match_repository, player_repository
    ):
        """Should return empty dict when no enrichment data exists."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        match_id = match_repository.record_match(
            team1_ids=[100],
            team2_ids=[200],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )

        # Add participant without enrichment
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000,
        )

        result = match_repository.get_bans_for_players([100], TEST_GUILD_ID)
        assert result == {}

    def test_get_bans_for_players_with_enrichment_data(
        self, match_repository, player_repository
    ):
        """Should only count bans from the opposing team."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        # Player 100 on team1 (Radiant), Player 200 on team2 (Dire)
        match_id = match_repository.record_match(
            team1_ids=[100],
            team2_ids=[200],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )

        # Add enrichment data with picks_bans
        # OpenDota team: 0=Radiant, 1=Dire
        enrichment_data = {
            "picks_bans": [
                {"is_pick": False, "hero_id": 10, "team": 0, "order": 0},  # Radiant ban (same team as player 100)
                {"is_pick": False, "hero_id": 20, "team": 1, "order": 1},  # Dire ban (opposing team for player 100)
                {"is_pick": True, "hero_id": 1, "team": 0, "order": 2},   # Pick (not a ban)
                {"is_pick": False, "hero_id": 30, "team": 1, "order": 3},  # Dire ban (opposing team for player 100)
            ]
        }

        match_repository.update_match_enrichment(
            match_id=match_id,
            valve_match_id=123456789,
            duration_seconds=2400,
            radiant_score=30,
            dire_score=20,
            game_mode=22,
            enrichment_data=json.dumps(enrichment_data),
        )

        # Add participant data
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000,
        )

        # Scouting player 100 (Radiant) - only Dire bans should count
        result = match_repository.get_bans_for_players([100], TEST_GUILD_ID)

        assert 10 not in result  # Radiant ban = same team, excluded
        assert 20 in result
        assert result[20] == 1  # Dire ban = opposing team
        assert 30 in result
        assert result[30] == 1  # Dire ban = opposing team
        assert 1 not in result  # Pick, not a ban

    def test_get_bans_deduplication_across_players(
        self, match_repository, player_repository
    ):
        """Should count bans only once per match even with multiple players."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=101, discord_username="Player1b", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        # Players 100 and 101 on Radiant (team1), player 200 on Dire (team2)
        match_id = match_repository.record_match(
            team1_ids=[100, 101],
            team2_ids=[200],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )

        # Dire ban (team=1) = opposing team for scouted Radiant players
        enrichment_data = {
            "picks_bans": [
                {"is_pick": False, "hero_id": 10, "team": 1, "order": 0},
            ]
        }
        match_repository.update_match_enrichment(
            match_id=match_id,
            valve_match_id=123456789,
            duration_seconds=2400,
            radiant_score=30,
            dire_score=20,
            game_mode=22,
            enrichment_data=json.dumps(enrichment_data),
        )

        match_repository.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000,
        )
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=101, hero_id=2,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000,
        )

        # Query with both players who were in the same match
        result = match_repository.get_bans_for_players([100, 101], TEST_GUILD_ID)

        # Should only count the ban once (match deduplication)
        assert result.get(10) == 1

    def test_get_bans_ignores_same_team_bans(
        self, match_repository, player_repository
    ):
        """Should not count bans made by the scouted player's own team."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        # Player 100 on Radiant (team1)
        match_id = match_repository.record_match(
            team1_ids=[100],
            team2_ids=[200],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )

        # Radiant ban (team=0) = same team as player 100
        enrichment_data = {
            "picks_bans": [
                {"is_pick": False, "hero_id": 10, "team": 0, "order": 0},
            ]
        }
        match_repository.update_match_enrichment(
            match_id=match_id,
            valve_match_id=123456789,
            duration_seconds=2400,
            radiant_score=30,
            dire_score=20,
            game_mode=22,
            enrichment_data=json.dumps(enrichment_data),
        )

        match_repository.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000,
        )

        result = match_repository.get_bans_for_players([100], TEST_GUILD_ID)

        # Radiant ban should NOT be counted when scouting a Radiant player
        assert result == {}


class TestScoutServiceMethod:
    """Tests for the scout service method."""

    def test_get_scout_data_empty_list(self, match_service):
        """Should return empty result for empty player list."""
        result = match_service.get_scout_data([], TEST_GUILD_ID)
        assert result == {"player_count": 0, "heroes": []}

    def test_get_scout_data_includes_total_matches(
        self, match_service, match_repository, player_repository
    ):
        """Should include total_matches in result."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        for _ in range(3):
            match_id = match_repository.record_match(
                team1_ids=[100], team2_ids=[200],
                winning_team=1, guild_id=TEST_GUILD_ID,
            )
            match_repository.update_participant_stats(
                match_id=match_id, discord_id=100, hero_id=1,
                kills=10, deaths=2, assists=5, gpm=600, xpm=500,
                hero_damage=20000, tower_damage=5000, last_hits=200,
                denies=10, net_worth=20000, lane_role=1,
            )

        result = match_service.get_scout_data([100], TEST_GUILD_ID)
        assert result["total_matches"] == 3

    def test_get_scout_data_sorts_by_total(
        self, match_service, match_repository, player_repository
    ):
        """Should sort heroes by total (games + bans), not just games."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        # Hero 1: 2 games, 0 bans → total 2
        for _ in range(2):
            match_id = match_repository.record_match(
                team1_ids=[100], team2_ids=[200],
                winning_team=1, guild_id=TEST_GUILD_ID,
            )
            match_repository.update_participant_stats(
                match_id=match_id, discord_id=100, hero_id=1,
                kills=10, deaths=2, assists=5, gpm=600, xpm=500,
                hero_damage=20000, tower_damage=5000, last_hits=200,
                denies=10, net_worth=20000, lane_role=1,
            )

        # Hero 2: 1 game + 3 opposing bans → total 4
        match_id = match_repository.record_match(
            team1_ids=[100], team2_ids=[200],
            winning_team=1, guild_id=TEST_GUILD_ID,
        )
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=2,
            kills=5, deaths=3, assists=7, gpm=400, xpm=400,
            hero_damage=15000, tower_damage=2000, last_hits=100,
            denies=5, net_worth=12000, lane_role=2,
        )

        # Add 3 matches with opposing-team bans on hero 2
        for _ in range(3):
            ban_match_id = match_repository.record_match(
                team1_ids=[100], team2_ids=[200],
                winning_team=1, guild_id=TEST_GUILD_ID,
            )
            match_repository.update_participant_stats(
                match_id=ban_match_id, discord_id=100, hero_id=1,
                kills=10, deaths=2, assists=5, gpm=600, xpm=500,
                hero_damage=20000, tower_damage=5000, last_hits=200,
                denies=10, net_worth=20000, lane_role=1,
            )
            enrichment = {
                "picks_bans": [
                    {"is_pick": False, "hero_id": 2, "team": 1, "order": 0},
                ]
            }
            match_repository.update_match_enrichment(
                match_id=ban_match_id, valve_match_id=100000 + ban_match_id,
                duration_seconds=2400, radiant_score=30, dire_score=20,
                game_mode=22, enrichment_data=json.dumps(enrichment),
            )

        result = match_service.get_scout_data([100], TEST_GUILD_ID)

        # Hero 1: 5 games + 0 bans = 5 total
        # Hero 2: 1 game + 3 bans = 4 total
        # Hero 1 should be first (higher total)
        assert result["heroes"][0]["hero_id"] == 1
        assert result["heroes"][1]["hero_id"] == 2
        assert result["heroes"][1]["bans"] == 3

    def test_get_scout_data_aggregation(
        self, match_service, match_repository, player_repository
    ):
        """Should aggregate hero stats across multiple players."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=300, discord_username="Player3", guild_id=TEST_GUILD_ID)

        # Player 100 plays hero 1
        match_id1 = match_repository.record_match(
            team1_ids=[100],
            team2_ids=[300],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )
        match_repository.update_participant_stats(
            match_id=match_id1, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000, lane_role=1,
        )

        # Player 200 also plays hero 1
        match_id2 = match_repository.record_match(
            team1_ids=[200],
            team2_ids=[300],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )
        match_repository.update_participant_stats(
            match_id=match_id2, discord_id=200, hero_id=1,
            kills=8, deaths=3, assists=7, gpm=550, xpm=480,
            hero_damage=18000, tower_damage=4000, last_hits=180,
            denies=8, net_worth=18000, lane_role=1,
        )

        result = match_service.get_scout_data([100, 200], TEST_GUILD_ID)

        assert result["player_count"] == 2
        assert len(result["heroes"]) >= 1

        # Find hero 1 in results
        hero_1_data = next((h for h in result["heroes"] if h["hero_id"] == 1), None)
        assert hero_1_data is not None
        assert hero_1_data["games"] == 2  # Aggregated across both players

    def test_get_scout_data_limit(
        self, match_service, match_repository, player_repository
    ):
        """Should respect the limit parameter."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        # Create matches with 15 different heroes
        for hero_id in range(1, 16):
            match_id = match_repository.record_match(
                team1_ids=[100],
                team2_ids=[200],
                winning_team=1,
                guild_id=TEST_GUILD_ID,
            )
            match_repository.update_participant_stats(
                match_id=match_id, discord_id=100, hero_id=hero_id,
                kills=10, deaths=2, assists=5, gpm=600, xpm=500,
                hero_damage=20000, tower_damage=5000, last_hits=200,
                denies=10, net_worth=20000, lane_role=1,
            )

        # Request with limit=5
        result = match_service.get_scout_data([100], TEST_GUILD_ID, limit=5)

        assert len(result["heroes"]) == 5

    def test_get_scout_data_includes_bans(
        self, match_service, match_repository, player_repository
    ):
        """Should include opposing-team ban counts in hero data."""
        player_repository.add(discord_id=100, discord_username="Player1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=200, discord_username="Player2", guild_id=TEST_GUILD_ID)

        # Player 100 on Radiant (team1)
        match_id = match_repository.record_match(
            team1_ids=[100],
            team2_ids=[200],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )

        # Dire ban (team=1) targeting hero 1 = opposing team for player 100
        enrichment_data = {
            "picks_bans": [
                {"is_pick": False, "hero_id": 1, "team": 1, "order": 0},
            ]
        }
        match_repository.update_match_enrichment(
            match_id=match_id,
            valve_match_id=123456789,
            duration_seconds=2400,
            radiant_score=30,
            dire_score=20,
            game_mode=22,
            enrichment_data=json.dumps(enrichment_data),
        )
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000, lane_role=1,
        )

        result = match_service.get_scout_data([100], TEST_GUILD_ID)

        hero_data = result["heroes"][0]
        assert hero_data["hero_id"] == 1
        assert hero_data["bans"] == 1


class TestScoutDrawing:
    """Tests for scout report drawing."""

    def test_draw_scout_report_empty_data(self):
        """Should handle empty data gracefully."""
        from utils.drawing import draw_scout_report

        result = draw_scout_report(
            scout_data={"player_count": 0, "total_matches": 0, "heroes": []},
            player_names=[],
            title="Test Scout",
        )

        assert result is not None
        # Should return a valid image
        assert result.getvalue().startswith(b"\x89PNG")

    def test_draw_scout_report_with_data(self):
        """Should generate a valid PNG image."""
        from utils.drawing import draw_scout_report

        scout_data = {
            "player_count": 2,
            "total_matches": 15,
            "heroes": [
                {"hero_id": 1, "games": 10, "wins": 7, "losses": 3, "bans": 2, "primary_role": 1},
                {"hero_id": 2, "games": 8, "wins": 4, "losses": 4, "bans": 0, "primary_role": 2},
            ],
        }

        result = draw_scout_report(
            scout_data=scout_data,
            player_names=["Player1", "Player2"],
            title="SCOUT: Radiant",
        )

        assert result is not None
        # Should return a valid PNG
        assert result.getvalue().startswith(b"\x89PNG")
