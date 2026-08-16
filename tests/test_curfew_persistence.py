"""Curfew window persistence and guild isolation (CurfewRepository)."""

from repositories.curfew_repository import CurfewRepository
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


class TestCurfewRepository:
    def test_list_for_player_starts_empty(self, repo_db_path):
        repo = CurfewRepository(repo_db_path)
        assert repo.list_for_player(1, TEST_GUILD_ID) == []

    def test_add_and_read_window(self, repo_db_path):
        repo = CurfewRepository(repo_db_path)
        repo.add_or_replace(
            1, TEST_GUILD_ID, "work",
            start_hour=9, start_minute=0, end_hour=17, end_minute=0, timezone=None,
        )

        windows = repo.list_for_player(1, TEST_GUILD_ID)
        assert len(windows) == 1
        window = windows[0]
        assert window.name == "work"
        assert window.start_hour == 9
        assert window.end_hour == 17
        assert window.timezone is None

    def test_multiple_named_windows_per_player(self, repo_db_path):
        repo = CurfewRepository(repo_db_path)
        repo.add_or_replace(1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0, timezone=None)
        repo.add_or_replace(1, TEST_GUILD_ID, "sleep", start_hour=22, start_minute=0, end_hour=6, end_minute=0, timezone=None)

        windows = repo.list_for_player(1, TEST_GUILD_ID)
        assert [w.name for w in windows] == ["sleep", "work"]  # ordered by name

    def test_add_or_replace_overwrites_same_name(self, repo_db_path):
        repo = CurfewRepository(repo_db_path)
        repo.add_or_replace(1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0, timezone=None)
        repo.add_or_replace(1, TEST_GUILD_ID, "work", start_hour=8, start_minute=30, end_hour=16, end_minute=30, timezone="America/Chicago")

        windows = repo.list_for_player(1, TEST_GUILD_ID)
        assert len(windows) == 1
        assert windows[0].start_hour == 8
        assert windows[0].start_minute == 30
        assert windows[0].timezone == "America/Chicago"

    def test_remove_existing_window_returns_true(self, repo_db_path):
        repo = CurfewRepository(repo_db_path)
        repo.add_or_replace(1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0, timezone=None)

        assert repo.remove(1, TEST_GUILD_ID, "work") is True
        assert repo.list_for_player(1, TEST_GUILD_ID) == []

    def test_remove_missing_window_returns_false(self, repo_db_path):
        repo = CurfewRepository(repo_db_path)
        assert repo.remove(1, TEST_GUILD_ID, "nonexistent") is False

    def test_windows_are_guild_scoped(self, repo_db_path):
        repo = CurfewRepository(repo_db_path)
        repo.add_or_replace(1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0, timezone=None)

        assert repo.list_for_player(1, TEST_GUILD_ID_SECONDARY) == []
        assert len(repo.list_for_player(1, TEST_GUILD_ID)) == 1

    def test_list_for_players_bulk_fetch(self, repo_db_path):
        repo = CurfewRepository(repo_db_path)
        repo.add_or_replace(1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0, timezone=None)
        repo.add_or_replace(2, TEST_GUILD_ID, "sleep", start_hour=22, start_minute=0, end_hour=6, end_minute=0, timezone=None)
        # Player 3 has no windows and should be absent from the result.

        result = repo.list_for_players([1, 2, 3], TEST_GUILD_ID)

        assert set(result.keys()) == {1, 2}
        assert result[1][0].name == "work"
        assert result[2][0].name == "sleep"

    def test_list_for_players_empty_input(self, repo_db_path):
        repo = CurfewRepository(repo_db_path)
        assert repo.list_for_players([], TEST_GUILD_ID) == {}
