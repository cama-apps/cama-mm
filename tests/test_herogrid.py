"""
Tests for the /herogrid command: repository methods, drawing function, and integration.
"""

from io import BytesIO
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest
from PIL import Image

from commands.herogrid import HeroGridCommands
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


# ---------------------------------------------------------------------------
# Player resolution priority chain tests
# ---------------------------------------------------------------------------


def _make_cog(
    lobby_players=None,
    conditional_players=None,
    shuffle_state=None,
    draft_pool_ids=None,
    last_match_ids=None,
    enriched_players=None,
    has_draft_state_manager=None,
):
    """Build a HeroGridCommands cog with mocked dependencies."""
    bot = SimpleNamespace()

    # Lobby manager
    lobby_manager = MagicMock()
    if lobby_players is not None:
        lobby = SimpleNamespace(
            players=set(lobby_players),
            conditional_players=set(conditional_players or []),
        )
        lobby_manager.get_lobby.return_value = lobby
    else:
        lobby_manager.get_lobby.return_value = None

    # Match service
    match_service = MagicMock()
    match_service.get_last_shuffle.return_value = shuffle_state

    # Draft state manager
    if draft_pool_ids is not None:
        dsm = MagicMock()
        dsm.get_state.return_value = SimpleNamespace(player_pool_ids=draft_pool_ids)
        bot.draft_state_manager = dsm

    # Match repo
    match_repo = MagicMock()
    match_repo.get_last_match_participant_ids.return_value = set(last_match_ids or [])
    match_repo.get_players_with_enriched_data.return_value = [
        {"discord_id": pid} for pid in (enriched_players or [])
    ]

    player_repo = MagicMock()

    return HeroGridCommands(bot, match_repo, player_repo, lobby_manager, match_service)


class TestResolvePlayerIds:
    def test_resolve_lobby_priority(self):
        """Lobby with players is highest priority."""
        cog = _make_cog(
            lobby_players=[1, 2, 3],
            shuffle_state={"radiant_team_ids": [10, 11], "dire_team_ids": [20, 21]},
        )
        ids, label = cog._resolve_player_ids("auto", guild_id=99)
        assert set(ids) == {1, 2, 3}
        assert label == "Lobby"

    def test_resolve_pending_match_fallback(self):
        """No lobby, pending match exists -> uses match IDs."""
        cog = _make_cog(
            shuffle_state={
                "radiant_team_ids": [1, 2, 3, 4, 5],
                "dire_team_ids": [6, 7, 8, 9, 10],
            },
        )
        ids, label = cog._resolve_player_ids("auto", guild_id=99)
        assert set(ids) == set(range(1, 11))
        assert label == "Active Match"

    def test_resolve_draft_fallback(self):
        """No lobby or match, active draft -> uses draft pool IDs."""
        cog = _make_cog(draft_pool_ids=[100, 200, 300])
        ids, label = cog._resolve_player_ids("auto", guild_id=99)
        assert set(ids) == {100, 200, 300}
        assert label == "Draft"

    def test_resolve_last_match_fallback(self):
        """No lobby/match/draft -> uses most recent match IDs."""
        cog = _make_cog(last_match_ids=[50, 51, 52, 53, 54])
        ids, label = cog._resolve_player_ids("auto", guild_id=99)
        assert set(ids) == {50, 51, 52, 53, 54}
        assert label == "Last Match"

    def test_resolve_all_fallback(self):
        """Nothing at all, source=auto -> falls back to all enriched players."""
        cog = _make_cog(enriched_players=[1000, 2000, 3000])
        ids, label = cog._resolve_player_ids("auto", guild_id=99)
        assert ids == [1000, 2000, 3000]
        assert label is None

    def test_resolve_priority_order(self):
        """Lobby AND pending match both present -> lobby wins."""
        cog = _make_cog(
            lobby_players=[1, 2],
            shuffle_state={"radiant_team_ids": [10], "dire_team_ids": [20]},
            last_match_ids=[50, 51],
        )
        ids, label = cog._resolve_player_ids("auto", guild_id=99)
        assert set(ids) == {1, 2}
        assert label == "Lobby"

    def test_draft_state_manager_none(self):
        """No draft_state_manager attr on bot -> gracefully skips draft check."""
        cog = _make_cog(last_match_ids=[70, 71])
        # SimpleNamespace won't have draft_state_manager unless we add it
        assert not hasattr(cog.bot, "draft_state_manager")
        ids, label = cog._resolve_player_ids("auto", guild_id=99)
        assert set(ids) == {70, 71}
        assert label == "Last Match"

    def test_match_service_none(self):
        """match_service is None -> gracefully skips pending match check."""
        cog = _make_cog(last_match_ids=[80, 81])
        cog.match_service = None
        ids, label = cog._resolve_player_ids("auto", guild_id=99)
        assert set(ids) == {80, 81}
        assert label == "Last Match"

    def test_source_all_skips_chain(self):
        """source=all -> goes directly to all players, ignoring lobby."""
        cog = _make_cog(
            lobby_players=[1, 2, 3],
            enriched_players=[100, 200],
        )
        ids, label = cog._resolve_player_ids("all", guild_id=99)
        assert ids == [100, 200]
        assert label is None

    def test_source_lobby_fails_gracefully(self):
        """source=lobby with nothing found -> returns empty list."""
        cog = _make_cog()
        ids, label = cog._resolve_player_ids("lobby", guild_id=99)
        assert ids == []


# ---------------------------------------------------------------------------
# Repeat label tests
# ---------------------------------------------------------------------------


class TestRepeatLabels:
    @staticmethod
    def _make_grid_data(num_players, num_heroes):
        """Generate grid data for given counts."""
        data = []
        names = {}
        for pid in range(1, num_players + 1):
            for hid in range(1, num_heroes + 1):
                data.append({"discord_id": pid, "hero_id": hid, "games": 5, "wins": 3})
            names[pid] = f"P{pid}"
        return data, names

    def test_no_repeat_small_grid(self):
        """5 players, 5 heroes: no repeats, dimensions match old formula."""
        data, names = self._make_grid_data(5, 5)
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        # With no repeats, width = 15 + 120 + 5*44 + 15 = 370
        # height = 15 + 30 + 90 + 5*44 + 80 + 15 = 450
        assert img.size == (370, 450)

    def test_repeat_bands_many_players(self):
        """25 players: image is taller than without repeats."""
        data, names = self._make_grid_data(25, 5)
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        # n_extra_bands = (25-1) // 10 = 2
        # No-repeat height would be: 15 + 30 + 90 + 25*44 + 80 + 15 = 1330
        # With repeats: 1330 + 2*90 = 1510
        assert img.size[1] == 1510

    def test_repeat_cols_many_heroes(self):
        """25 heroes: image is wider than without repeats."""
        data, names = self._make_grid_data(5, 25)
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        # n_extra_cols = (25-1) // 10 = 2
        # No-repeat width: 15 + 120 + 25*44 + 15 = 1250
        # With repeats: 1250 + 2*120 = 1490
        assert img.size[0] == 1490

    def test_exactly_10_no_repeat(self):
        """10 players, 10 heroes: no repeats (boundary condition)."""
        data, names = self._make_grid_data(10, 10)
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        # No repeats: width = 15 + 120 + 10*44 + 15 = 590
        # height = 15 + 30 + 90 + 10*44 + 80 + 15 = 670
        assert img.size == (590, 670)

    def test_11_players_one_repeat(self):
        """11 players: one extra hero header band."""
        data, names = self._make_grid_data(11, 5)
        result = draw_hero_grid(data, names, min_games=1)
        img = Image.open(result)
        # n_extra_bands = (11-1) // 10 = 1
        # No-repeat height: 15 + 30 + 90 + 11*44 + 80 + 15 = 714
        # With repeats: 714 + 1*90 = 804
        assert img.size[1] == 804
