"""
Tests for the /scout command functionality.
"""

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

from commands.scout import ScoutCommands
from services.player_service import PlayerService
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


class TestGetSteamIdsBulk:
    """Tests for PlayerService.get_steam_ids_bulk, used by /scout links."""

    def test_empty_list_returns_empty_dict(self, player_repository):
        """An empty input should produce an empty mapping."""
        service = PlayerService(player_repository)
        assert service.get_steam_ids_bulk([]) == {}

    def test_returns_all_linked_accounts(self, player_repository):
        """Every linked Steam account for each player is returned, primary first."""
        player_repository.add(
            discord_id=100, discord_username="Solo", guild_id=TEST_GUILD_ID
        )
        player_repository.add(
            discord_id=200, discord_username="Smurf", guild_id=TEST_GUILD_ID
        )
        player_repository.add_steam_id(100, 111111, is_primary=True)
        player_repository.add_steam_id(200, 222222, is_primary=True)
        player_repository.add_steam_id(200, 333333, is_primary=False)

        result = PlayerService(player_repository).get_steam_ids_bulk([100, 200])

        assert result[100] == [111111]
        assert set(result[200]) == {222222, 333333}
        assert result[200][0] == 222222  # primary account listed first

    def test_player_without_steam_id_has_empty_list(self, player_repository):
        """A registered player with no linked account maps to an empty list."""
        player_repository.add(
            discord_id=100, discord_username="NoSteam", guild_id=TEST_GUILD_ID
        )
        result = PlayerService(player_repository).get_steam_ids_bulk([100])
        assert result == {100: []}


def _make_scout_cog(
    *,
    match_service=None,
    draft_state_manager=None,
    lobby=None,
    player_service=None,
):
    """Build a ScoutCommands cog with stub collaborators for resolver/command tests."""
    bot = SimpleNamespace()
    if draft_state_manager is not None:
        bot.draft_state_manager = draft_state_manager
    lobby_manager = SimpleNamespace(get_lobby=lambda guild_id=None: lobby)
    return ScoutCommands(bot, match_service, player_service, lobby_manager)


def _match_service(radiant, dire):
    """A stub match service whose get_last_shuffle returns a post-shuffle state."""
    shuffle = SimpleNamespace(radiant_team_ids=radiant, dire_team_ids=dire)
    return SimpleNamespace(get_last_shuffle=lambda guild_id: shuffle)


def _links_cmd(cog):
    """Return the /scout links Command object from a ScoutCommands cog."""
    return next(c for c in cog.scout.commands if c.name == "links")


class TestResolveTeamContext:
    """Tests for ScoutCommands._resolve_team_context (used by /scout links)."""

    def test_no_context_returns_empty(self):
        """With no match, draft, or lobby, every list is empty."""
        ctx = _make_scout_cog()._resolve_team_context(TEST_GUILD_ID)
        assert ctx.flat == []
        assert ctx.radiant == []
        assert ctx.dire == []
        assert ctx.split is False
        assert ctx.source_label is None

    def test_lobby_has_no_team_split(self):
        """An open lobby yields a flat list with split=False."""
        lobby = SimpleNamespace(players=[5, 6, 7], conditional_players=[])
        ctx = _make_scout_cog(lobby=lobby)._resolve_team_context(TEST_GUILD_ID)
        assert ctx.split is False
        assert ctx.flat == [5, 6, 7]
        assert ctx.source_label == "Lobby"

    def test_lobby_includes_conditional_players(self):
        """Conditional (frogling) players are appended to the lobby list."""
        lobby = SimpleNamespace(players=[5, 6], conditional_players=[9])
        ctx = _make_scout_cog(lobby=lobby)._resolve_team_context(TEST_GUILD_ID)
        assert ctx.flat == [5, 6, 9]

    def test_pending_shuffle_splits_teams(self):
        """A post-shuffle pending match yields radiant/dire with split=True."""
        cog = _make_scout_cog(match_service=_match_service([1, 2, 3], [4, 5, 6]))
        ctx = cog._resolve_team_context(TEST_GUILD_ID)
        assert ctx.split is True
        assert ctx.radiant == [1, 2, 3]
        assert ctx.dire == [4, 5, 6]
        assert ctx.flat == [1, 2, 3, 4, 5, 6]
        assert ctx.source_label == "Active Match"

    def test_pending_shuffle_takes_priority_over_lobby(self):
        """A pending match wins over an open lobby (priority order)."""
        cog = _make_scout_cog(
            match_service=_match_service([1, 2], [3, 4]),
            lobby=SimpleNamespace(players=[90, 91], conditional_players=[]),
        )
        ctx = cog._resolve_team_context(TEST_GUILD_ID)
        assert ctx.split is True
        assert ctx.source_label == "Active Match"
        assert ctx.flat == [1, 2, 3, 4]


class TestResolvePlayerContextRegression:
    """_resolve_player_context must keep its (ids, label) contract for /scout report."""

    def test_returns_flat_list_and_label(self):
        """With no team filter, both teams are flattened under the source label."""
        cog = _make_scout_cog(match_service=_match_service([1, 2], [3, 4]))
        assert cog._resolve_player_context(TEST_GUILD_ID) == ([1, 2, 3, 4], "Active Match")

    def test_team_filter_narrows_to_one_team(self):
        """A radiant/dire filter narrows the result and relabels it."""
        cog = _make_scout_cog(match_service=_match_service([1, 2], [3, 4]))
        assert cog._resolve_player_context(TEST_GUILD_ID, "radiant") == ([1, 2], "Radiant")
        assert cog._resolve_player_context(TEST_GUILD_ID, "dire") == ([3, 4], "Dire")

    def test_draft_filter_keeps_draft_prefix(self):
        """Filtering a draft keeps the 'Draft Radiant' / 'Draft Dire' labels."""
        draft = SimpleNamespace(
            radiant_player_ids=[1, 2], dire_player_ids=[3, 4], player_pool_ids=[]
        )
        cog = _make_scout_cog(
            draft_state_manager=SimpleNamespace(get_state=lambda guild_id: draft)
        )
        assert cog._resolve_player_context(TEST_GUILD_ID, "radiant") == (
            [1, 2],
            "Draft Radiant",
        )
        assert cog._resolve_player_context(TEST_GUILD_ID, "dire") == ([3, 4], "Draft Dire")

    def test_no_context_returns_empty_tuple(self):
        """With no context the wrapper returns ([], None)."""
        assert _make_scout_cog()._resolve_player_context(TEST_GUILD_ID) == ([], None)


class TestBuildLinkLines:
    """Tests for ScoutCommands._build_link_lines (Dotabuff link rendering)."""

    def _cog(self):
        return ScoutCommands(None, None, None, None)

    def test_single_account_links_to_dotabuff(self):
        """A player with one Steam account gets one Dotabuff link."""
        lines = self._cog()._build_link_lines([100], {100: "Alice"}, {100: [111]})
        assert lines == ["**Alice** — [Dotabuff](https://www.dotabuff.com/players/111)"]

    def test_multiple_accounts_are_all_listed_and_numbered(self):
        """A player with smurfs gets one numbered link per account."""
        lines = self._cog()._build_link_lines([200], {200: "Bob"}, {200: [222, 333]})
        assert lines == [
            "**Bob** — [Dotabuff 1](https://www.dotabuff.com/players/222) · "
            "[Dotabuff 2](https://www.dotabuff.com/players/333)"
        ]

    def test_player_without_account_is_noted(self):
        """A player with no linked account is still listed, with a note."""
        lines = self._cog()._build_link_lines([300], {300: "Carol"}, {300: []})
        assert lines == ["**Carol** — no linked Steam account"]

    def test_unknown_name_falls_back_to_mention(self):
        """A player missing from the name map is shown as a Discord mention."""
        lines = self._cog()._build_link_lines([400], {}, {400: [444]})
        assert lines == ["**<@400>** — [Dotabuff](https://www.dotabuff.com/players/444)"]


class TestScoutLinksCommand:
    """Integration tests for the /scout links command handler."""

    def _patch_interaction_safety(self, monkeypatch):
        """Patch safe_defer/safe_followup; return the safe_followup AsyncMock."""
        followup = AsyncMock()
        monkeypatch.setattr("commands.scout.safe_defer", AsyncMock(return_value=True))
        monkeypatch.setattr("commands.scout.safe_followup", followup)
        return followup

    async def test_no_context_reports_no_players(self, monkeypatch):
        """With nothing active, the command explains how to use it."""
        followup = self._patch_interaction_safety(monkeypatch)
        cog = _make_scout_cog()
        interaction = SimpleNamespace(guild=SimpleNamespace(id=TEST_GUILD_ID))

        await _links_cmd(cog).callback(cog, interaction)

        followup.assert_awaited_once()
        assert "No players found" in followup.await_args.kwargs["content"]

    async def test_pending_shuffle_builds_two_team_embed(self, monkeypatch):
        """A pending match produces an embed with Radiant and Dire link fields."""
        followup = self._patch_interaction_safety(monkeypatch)
        player_service = SimpleNamespace(
            get_steam_ids_bulk=lambda ids: {1: [11], 2: [22], 3: [33], 4: [44]},
            get_by_ids=lambda ids, guild_id: [
                SimpleNamespace(discord_id=1, name="R1"),
                SimpleNamespace(discord_id=2, name="R2"),
                SimpleNamespace(discord_id=3, name="D1"),
                SimpleNamespace(discord_id=4, name="D2"),
            ],
        )
        cog = _make_scout_cog(
            match_service=_match_service([1, 2], [3, 4]),
            player_service=player_service,
        )
        interaction = SimpleNamespace(guild=SimpleNamespace(id=TEST_GUILD_ID))

        await _links_cmd(cog).callback(cog, interaction)

        followup.assert_awaited_once()
        embed = followup.await_args.kwargs["embed"]
        assert [f.name for f in embed.fields] == ["Radiant", "Dire"]
        assert "R1" in embed.fields[0].value
        assert "https://www.dotabuff.com/players/11" in embed.fields[0].value
        assert "https://www.dotabuff.com/players/33" in embed.fields[1].value

    async def test_explicit_mentions_list_those_players(self, monkeypatch):
        """Passing @mentions lists exactly those players in a single field."""
        followup = self._patch_interaction_safety(monkeypatch)
        player_service = SimpleNamespace(
            get_steam_ids_bulk=lambda ids: {7: [77]},
            get_by_ids=lambda ids, guild_id: [SimpleNamespace(discord_id=7, name="Solo")],
        )
        cog = _make_scout_cog(player_service=player_service)
        interaction = SimpleNamespace(guild=SimpleNamespace(id=TEST_GUILD_ID))

        await _links_cmd(cog).callback(cog, interaction, players="<@7>")

        followup.assert_awaited_once()
        embed = followup.await_args.kwargs["embed"]
        assert [f.name for f in embed.fields] == ["Players"]
        assert "https://www.dotabuff.com/players/77" in embed.fields[0].value
