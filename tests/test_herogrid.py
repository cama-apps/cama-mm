"""
Tests for the /herogrid command: repository methods, drawing function, and integration.
"""

from io import BytesIO

import pytest
from PIL import Image

from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from utils.drawing import draw_hero_grid


# ---------------------------------------------------------------------------
# Repository tests
# ---------------------------------------------------------------------------


class TestGetMultiPlayerHeroStats:
    def test_empty_ids_returns_empty(self, match_repository):
        result = match_repository.get_multi_player_hero_stats([])
        assert result == []

    def test_no_enriched_data_returns_empty(self, match_repository, player_repository):
        """Players with matches but no enrichment should return nothing."""
        player_repository.add(discord_id=100, discord_username="Alice")
        player_repository.add(discord_id=200, discord_username="Bob")
        match_repository.record_match(
            team1_ids=[100], team2_ids=[200], winning_team=1
        )
        result = match_repository.get_multi_player_hero_stats([100, 200])
        assert result == []

    def test_single_player_with_data(self, match_repository, player_repository):
        player_repository.add(discord_id=100, discord_username="Alice")
        player_repository.add(discord_id=200, discord_username="Bob")
        match_id = match_repository.record_match(
            team1_ids=[100], team2_ids=[200], winning_team=1
        )
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000,
        )
        result = match_repository.get_multi_player_hero_stats([100])
        assert len(result) == 1
        assert result[0]["discord_id"] == 100
        assert result[0]["hero_id"] == 1
        assert result[0]["games"] == 1
        assert result[0]["wins"] == 1

    def test_multiple_players(self, match_repository, player_repository):
        player_repository.add(discord_id=100, discord_username="Alice")
        player_repository.add(discord_id=200, discord_username="Bob")
        match_id = match_repository.record_match(
            team1_ids=[100], team2_ids=[200], winning_team=1
        )
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000,
        )
        match_repository.update_participant_stats(
            match_id=match_id, discord_id=200, hero_id=2,
            kills=5, deaths=8, assists=12, gpm=400, xpm=400,
            hero_damage=15000, tower_damage=2000, last_hits=100,
            denies=5, net_worth=12000,
        )
        result = match_repository.get_multi_player_hero_stats([100, 200])
        assert len(result) == 2
        discord_ids = {r["discord_id"] for r in result}
        assert discord_ids == {100, 200}

    def test_aggregates_across_matches(self, match_repository, player_repository):
        player_repository.add(discord_id=100, discord_username="Alice")
        player_repository.add(discord_id=200, discord_username="Bob")

        # Match 1: Alice wins on hero 1
        m1 = match_repository.record_match(
            team1_ids=[100], team2_ids=[200], winning_team=1
        )
        match_repository.update_participant_stats(
            match_id=m1, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000,
        )
        # Match 2: Alice loses on hero 1
        m2 = match_repository.record_match(
            team1_ids=[200], team2_ids=[100], winning_team=1
        )
        match_repository.update_participant_stats(
            match_id=m2, discord_id=100, hero_id=1,
            kills=5, deaths=8, assists=3, gpm=400, xpm=400,
            hero_damage=15000, tower_damage=2000, last_hits=100,
            denies=5, net_worth=12000,
        )
        # Match 3: Alice wins on hero 1
        m3 = match_repository.record_match(
            team1_ids=[100], team2_ids=[200], winning_team=1
        )
        match_repository.update_participant_stats(
            match_id=m3, discord_id=100, hero_id=1,
            kills=12, deaths=1, assists=8, gpm=700, xpm=600,
            hero_damage=25000, tower_damage=7000, last_hits=250,
            denies=15, net_worth=25000,
        )

        result = match_repository.get_multi_player_hero_stats([100])
        assert len(result) == 1
        assert result[0]["games"] == 3
        assert result[0]["wins"] == 2


class TestGetPlayersWithEnrichedData:
    def test_empty_db(self, match_repository):
        result = match_repository.get_players_with_enriched_data()
        assert result == []

    def test_returns_sorted_by_games(self, match_repository, player_repository):
        player_repository.add(discord_id=100, discord_username="Alice")
        player_repository.add(discord_id=200, discord_username="Bob")

        # Alice: 2 enriched matches
        for i in range(2):
            m = match_repository.record_match(
                team1_ids=[100], team2_ids=[200], winning_team=1
            )
            match_repository.update_participant_stats(
                match_id=m, discord_id=100, hero_id=1,
                kills=10, deaths=2, assists=5, gpm=600, xpm=500,
                hero_damage=20000, tower_damage=5000, last_hits=200,
                denies=10, net_worth=20000,
            )

        # Bob: 1 enriched match
        m = match_repository.record_match(
            team1_ids=[200], team2_ids=[100], winning_team=1
        )
        match_repository.update_participant_stats(
            match_id=m, discord_id=200, hero_id=2,
            kills=5, deaths=3, assists=10, gpm=400, xpm=400,
            hero_damage=15000, tower_damage=2000, last_hits=100,
            denies=5, net_worth=12000,
        )

        result = match_repository.get_players_with_enriched_data()
        assert len(result) == 2
        assert result[0]["discord_id"] == 100  # More games first
        assert result[0]["total_games"] == 2
        assert result[1]["discord_id"] == 200
        assert result[1]["total_games"] == 1

    def test_excludes_players_without_hero_data(self, match_repository, player_repository):
        player_repository.add(discord_id=100, discord_username="Alice")
        player_repository.add(discord_id=200, discord_username="Bob")

        # Match with no enrichment
        match_repository.record_match(
            team1_ids=[100], team2_ids=[200], winning_team=1
        )

        result = match_repository.get_players_with_enriched_data()
        assert result == []


# ---------------------------------------------------------------------------
# Drawing tests
# ---------------------------------------------------------------------------


class TestDrawHeroGrid:
    def test_empty_data_returns_image(self):
        result = draw_hero_grid([], {})
        assert isinstance(result, BytesIO)
        img = Image.open(result)
        assert img.format == "PNG"

    def test_empty_player_names_returns_image(self):
        data = [{"discord_id": 1, "hero_id": 1, "games": 5, "wins": 3}]
        result = draw_hero_grid(data, {})
        img = Image.open(result)
        assert img.format == "PNG"

    def test_single_player_single_hero(self):
        data = [{"discord_id": 1, "hero_id": 1, "games": 5, "wins": 3}]
        names = {1: "TestPlayer"}
        result = draw_hero_grid(data, names, min_games=1)
        assert isinstance(result, BytesIO)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size[0] > 0 and img.size[1] > 0

    def test_multiple_players_multiple_heroes(self):
        data = [
            {"discord_id": 1, "hero_id": 1, "games": 10, "wins": 7},
            {"discord_id": 1, "hero_id": 2, "games": 5, "wins": 1},
            {"discord_id": 2, "hero_id": 1, "games": 3, "wins": 3},
            {"discord_id": 2, "hero_id": 3, "games": 8, "wins": 4},
        ]
        names = {1: "Alice", 2: "Bob"}
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.mode == "RGBA"

    def test_min_games_filters_heroes(self):
        data = [
            {"discord_id": 1, "hero_id": 1, "games": 5, "wins": 3},
            {"discord_id": 1, "hero_id": 2, "games": 1, "wins": 0},
        ]
        names = {1: "TestPlayer"}
        # With min_games=2, hero_id=2 (1 game) should be filtered out
        result_filtered = draw_hero_grid(data, names, min_games=2)
        img_filtered = Image.open(result_filtered)

        # With min_games=1, both heroes should appear
        result_all = draw_hero_grid(data, names, min_games=1)
        img_all = Image.open(result_all)

        # Filtered image should be narrower (fewer hero columns)
        assert img_filtered.size[0] < img_all.size[0]

    def test_no_heroes_meet_threshold(self):
        data = [{"discord_id": 1, "hero_id": 1, "games": 1, "wins": 0}]
        names = {1: "TestPlayer"}
        result = draw_hero_grid(data, names, min_games=5)
        img = Image.open(result)
        assert img.format == "PNG"

    def test_hero_cap_width(self):
        # Generate data with many heroes
        data = [
            {"discord_id": 1, "hero_id": i, "games": 3, "wins": 1}
            for i in range(1, 81)
        ]
        names = {1: "TestPlayer"}
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size[0] <= 4000

    def test_winrate_colors_all_brackets(self):
        data = [
            {"discord_id": 1, "hero_id": 1, "games": 10, "wins": 8},   # 80% green
            {"discord_id": 1, "hero_id": 2, "games": 10, "wins": 5},   # 50% light green
            {"discord_id": 1, "hero_id": 3, "games": 10, "wins": 4},   # 40% yellow
            {"discord_id": 1, "hero_id": 4, "games": 10, "wins": 2},   # 20% red
        ]
        names = {1: "TestPlayer"}
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        assert img.format == "PNG"

    def test_returns_seekable_bytesio(self):
        data = [{"discord_id": 1, "hero_id": 1, "games": 5, "wins": 3}]
        names = {1: "Test"}
        result = draw_hero_grid(data, names, min_games=1)
        assert isinstance(result, BytesIO)
        assert result.tell() == 0

    def test_long_player_name_truncated(self):
        data = [{"discord_id": 1, "hero_id": 1, "games": 5, "wins": 3}]
        names = {1: "ThisIsAVeryLongPlayerName"}
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        assert img.format == "PNG"

    def test_many_players(self):
        data = []
        names = {}
        for pid in range(1, 21):
            data.append({"discord_id": pid, "hero_id": 1, "games": 5, "wins": 3})
            names[pid] = f"Player{pid}"
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size[1] > 0

    def test_player_order_preserved(self):
        """Players should appear in the order specified by player_names keys."""
        data = [
            {"discord_id": 1, "hero_id": 1, "games": 5, "wins": 3},
            {"discord_id": 2, "hero_id": 1, "games": 10, "wins": 8},
        ]
        # Specify order: player 2 first, then player 1
        names = {2: "Bob", 1: "Alice"}
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        assert img.format == "PNG"


# ---------------------------------------------------------------------------
# Integration test
# ---------------------------------------------------------------------------


class TestHeroGridIntegration:
    def test_full_pipeline(self, repo_db_path):
        """Test the full data flow: insert data, query, generate image."""
        player_repo = PlayerRepository(repo_db_path)
        match_repo = MatchRepository(repo_db_path)

        # Register players
        player_repo.add(discord_id=100, discord_username="Alice")
        player_repo.add(discord_id=200, discord_username="Bob")

        # Record and enrich a match
        match_id = match_repo.record_match(
            team1_ids=[100], team2_ids=[200], winning_team=1
        )
        match_repo.update_participant_stats(
            match_id=match_id, discord_id=100, hero_id=1,
            kills=10, deaths=2, assists=5, gpm=600, xpm=500,
            hero_damage=20000, tower_damage=5000, last_hits=200,
            denies=10, net_worth=20000,
        )
        match_repo.update_participant_stats(
            match_id=match_id, discord_id=200, hero_id=2,
            kills=5, deaths=8, assists=12, gpm=400, xpm=400,
            hero_damage=15000, tower_damage=2000, last_hits=100,
            denies=5, net_worth=12000,
        )

        # Query
        grid_data = match_repo.get_multi_player_hero_stats([100, 200])
        assert len(grid_data) == 2

        # Build player names
        players = player_repo.get_by_ids([100, 200])
        player_names = {p.discord_id: p.name for p in players}

        # Generate image
        result = draw_hero_grid(grid_data, player_names, min_games=1)
        assert isinstance(result, BytesIO)
        img = Image.open(result)
        assert img.format == "PNG"
        assert img.size[0] > 0 and img.size[1] > 0
