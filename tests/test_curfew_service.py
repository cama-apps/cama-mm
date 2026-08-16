"""Tests for CurfewService: window management, active-window lookup, and the sweep."""

from datetime import datetime
from zoneinfo import ZoneInfo

import pytest

from domain.models.lobby import LobbyKind
from repositories.curfew_repository import CurfewRepository
from services.curfew_service import CurfewService
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService
from tests.conftest import TEST_GUILD_ID
from tests.fakes.lobby_repo import FakeLobbyRepo
from tests.test_dual_lobby_queue import FakePlayerRepo, seat

ELEVEN_PM_ET = datetime(2026, 1, 1, 23, 0, tzinfo=ZoneInfo("America/New_York"))
NOON_ET = datetime(2026, 1, 1, 12, 0, tzinfo=ZoneInfo("America/New_York"))


def make_services(repo_db_path, player_ids=()):
    manager = LobbyManager(FakeLobbyRepo())
    player_repo = FakePlayerRepo()
    for discord_id in player_ids:
        player_repo.add_player(discord_id)
    lobby_service = LobbyService(manager, player_repo)
    for kind in LobbyKind:
        manager.get_or_create_lobby(guild_id=TEST_GUILD_ID, lobby_kind=kind)
    curfew_repo = CurfewRepository(repo_db_path)
    curfew_service = CurfewService(player_repo, curfew_repo, lobby_service)
    return lobby_service, player_repo, curfew_repo, curfew_service


class TestAddWindow:
    def test_add_window_persists(self, repo_db_path):
        _, player_repo, curfew_repo, curfew_service = make_services(repo_db_path, player_ids=[1])

        window = curfew_service.add_window(
            1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0
        )

        assert window.name == "work"
        assert curfew_repo.list_for_player(1, TEST_GUILD_ID)[0].name == "work"

    def test_add_window_strips_name(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])

        window = curfew_service.add_window(
            1, TEST_GUILD_ID, "  work  ", start_hour=9, start_minute=0, end_hour=17, end_minute=0
        )

        assert window.name == "work"

    def test_add_window_rejects_empty_name(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])

        with pytest.raises(ValueError, match="name"):
            curfew_service.add_window(
                1, TEST_GUILD_ID, "   ", start_hour=9, start_minute=0, end_hour=17, end_minute=0
            )

    def test_add_window_rejects_too_long_name(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])

        with pytest.raises(ValueError, match="40"):
            curfew_service.add_window(
                1, TEST_GUILD_ID, "x" * 41, start_hour=9, start_minute=0, end_hour=17, end_minute=0
            )

    def test_add_window_rejects_out_of_range_hour(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])

        with pytest.raises(ValueError, match="Hour"):
            curfew_service.add_window(
                1, TEST_GUILD_ID, "work", start_hour=24, start_minute=0, end_hour=17, end_minute=0
            )

    def test_add_window_rejects_equal_start_and_end(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])

        with pytest.raises(ValueError, match="same"):
            curfew_service.add_window(
                1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=9, end_minute=0
            )

    def test_add_window_rejects_unknown_timezone(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])

        with pytest.raises(ValueError, match="timezone"):
            curfew_service.add_window(
                1, TEST_GUILD_ID, "work",
                start_hour=9, start_minute=0, end_hour=17, end_minute=0,
                timezone="Not/AZone",
            )

    def test_add_window_unregistered_player_raises(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[])

        with pytest.raises(ValueError, match="not registered"):
            curfew_service.add_window(
                999, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0
            )

    def test_add_window_replaces_existing_name(self, repo_db_path):
        _, _, curfew_repo, curfew_service = make_services(repo_db_path, player_ids=[1])
        curfew_service.add_window(1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0)

        curfew_service.add_window(1, TEST_GUILD_ID, "work", start_hour=8, start_minute=0, end_hour=16, end_minute=0)

        windows = curfew_repo.list_for_player(1, TEST_GUILD_ID)
        assert len(windows) == 1
        assert windows[0].start_hour == 8


class TestRemoveAndListWindows:
    def test_remove_window(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        curfew_service.add_window(1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0)

        assert curfew_service.remove_window(1, TEST_GUILD_ID, "work") is True
        assert curfew_service.list_windows(1, TEST_GUILD_ID) == []

    def test_remove_missing_window(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        assert curfew_service.remove_window(1, TEST_GUILD_ID, "nope") is False

    def test_list_windows_multiple(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        curfew_service.add_window(1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0)
        curfew_service.add_window(1, TEST_GUILD_ID, "sleep", start_hour=22, start_minute=0, end_hour=6, end_minute=0)

        names = {w.name for w in curfew_service.list_windows(1, TEST_GUILD_ID)}
        assert names == {"work", "sleep"}


class TestActiveWindow:
    def test_returns_none_with_no_windows(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        assert curfew_service.active_window(1, TEST_GUILD_ID, now=ELEVEN_PM_ET) is None

    def test_returns_matching_window(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        curfew_service.add_window(1, TEST_GUILD_ID, "sleep", start_hour=22, start_minute=0, end_hour=6, end_minute=0)

        match = curfew_service.active_window(1, TEST_GUILD_ID, now=ELEVEN_PM_ET)

        assert match.name == "sleep"

    def test_returns_none_outside_window(self, repo_db_path):
        _, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        curfew_service.add_window(1, TEST_GUILD_ID, "sleep", start_hour=22, start_minute=0, end_hour=6, end_minute=0)

        assert curfew_service.active_window(1, TEST_GUILD_ID, now=NOON_ET) is None

    def test_window_without_its_own_timezone_falls_back_to_players_general_timezone(self, repo_db_path):
        """A window added with timezone=None rides the player's /player timezone setting."""
        _, player_repo, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        player_repo.players[(1, TEST_GUILD_ID)].timezone = "Asia/Tokyo"
        curfew_service.add_window(
            1, TEST_GUILD_ID, "sleep", start_hour=22, start_minute=0, end_hour=6, end_minute=0
        )
        utc_now = datetime(2026, 1, 1, 13, 30, tzinfo=ZoneInfo("UTC"))  # 10:30pm JST

        match = curfew_service.active_window(1, TEST_GUILD_ID, now=utc_now)

        assert match is not None
        assert match.name == "sleep"


class TestSweep:
    def test_sweep_removes_only_players_in_active_windows(self, repo_db_path):
        lobby_service, player_repo, _, curfew_service = make_services(repo_db_path, player_ids=[1, 2])
        seat(lobby_service, 1, LobbyKind.OPEN)
        seat(lobby_service, 2, LobbyKind.OPEN)
        curfew_service.add_window(1, TEST_GUILD_ID, "sleep", start_hour=22, start_minute=0, end_hour=6, end_minute=0)

        kicks = curfew_service.sweep([TEST_GUILD_ID], now=ELEVEN_PM_ET)

        assert [k.discord_id for k in kicks] == [1]
        assert kicks[0].window_name == "sleep"
        assert kicks[0].lobby_kind is LobbyKind.OPEN
        assert 1 not in lobby_service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN).players
        assert 2 in lobby_service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN).players

    def test_sweep_ignores_players_outside_their_window(self, repo_db_path):
        lobby_service, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        seat(lobby_service, 1, LobbyKind.OPEN)
        curfew_service.add_window(1, TEST_GUILD_ID, "work", start_hour=9, start_minute=0, end_hour=17, end_minute=0)

        kicks = curfew_service.sweep([TEST_GUILD_ID], now=ELEVEN_PM_ET)

        assert kicks == []
        assert 1 in lobby_service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN).players

    def test_sweep_checks_every_lobby_kind(self, repo_db_path):
        lobby_service, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        seat(lobby_service, 1, LobbyKind.LOWSKILL)
        curfew_service.add_window(1, TEST_GUILD_ID, "sleep", start_hour=22, start_minute=0, end_hour=6, end_minute=0)

        kicks = curfew_service.sweep([TEST_GUILD_ID], now=ELEVEN_PM_ET)

        assert [k.lobby_kind for k in kicks] == [LobbyKind.LOWSKILL]

    def test_sweep_with_no_windows_set_is_a_noop(self, repo_db_path):
        lobby_service, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        seat(lobby_service, 1, LobbyKind.OPEN)

        assert curfew_service.sweep([TEST_GUILD_ID], now=ELEVEN_PM_ET) == []

    def test_sweep_scans_multiple_guilds(self, repo_db_path):
        lobby_service, _, _, curfew_service = make_services(repo_db_path, player_ids=[1])
        seat(lobby_service, 1, LobbyKind.OPEN)
        curfew_service.add_window(1, TEST_GUILD_ID, "sleep", start_hour=22, start_minute=0, end_hour=6, end_minute=0)

        kicks = curfew_service.sweep([TEST_GUILD_ID, 999999], now=ELEVEN_PM_ET)

        assert [k.guild_id for k in kicks] == [TEST_GUILD_ID]
