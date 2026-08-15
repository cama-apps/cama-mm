"""Tests for CurfewService.sweep — removing curfewed players from active lobbies."""

from datetime import datetime
from zoneinfo import ZoneInfo

from domain.models.lobby import LobbyKind
from services.curfew_service import CurfewService
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService
from tests.conftest import TEST_GUILD_ID
from tests.fakes.lobby_repo import FakeLobbyRepo
from tests.test_dual_lobby_queue import FakePlayerRepo, seat

# 11pm ET, safely inside a 10pm-6am curfew window and outside a 1am-5am one.
ELEVEN_PM_ET = datetime(2026, 1, 1, 23, 0, tzinfo=ZoneInfo("America/New_York"))


def make_curfew_services(player_ids=()):
    manager = LobbyManager(FakeLobbyRepo())
    player_repo = FakePlayerRepo()
    for discord_id in player_ids:
        player_repo.add_player(discord_id)
    lobby_service = LobbyService(manager, player_repo)
    for kind in LobbyKind:
        manager.get_or_create_lobby(guild_id=TEST_GUILD_ID, lobby_kind=kind)
    return lobby_service, player_repo, CurfewService(player_repo, lobby_service)


def _enable_curfew(player_repo, discord_id, *, curfew_hour=22, wake_hour=6, timezone="America/New_York"):
    player = player_repo.players[(discord_id, TEST_GUILD_ID)]
    player.curfew_enabled = True
    player.curfew_hour = curfew_hour
    player.curfew_minute = 0
    player.curfew_wake_hour = wake_hour
    player.curfew_wake_minute = 0
    player.curfew_timezone = timezone


class TestCurfewSweep:
    def test_sweep_removes_only_curfewed_players(self):
        lobby_service, player_repo, curfew_service = make_curfew_services(player_ids=[1, 2])
        seat(lobby_service, 1, LobbyKind.OPEN)
        seat(lobby_service, 2, LobbyKind.OPEN)
        _enable_curfew(player_repo, 1)

        kicks = curfew_service.sweep([TEST_GUILD_ID], now=ELEVEN_PM_ET)

        assert [k.discord_id for k in kicks] == [1]
        assert kicks[0].lobby_kind is LobbyKind.OPEN
        assert 1 not in lobby_service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN).players
        assert 2 in lobby_service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN).players

    def test_sweep_ignores_players_outside_their_window(self):
        lobby_service, player_repo, curfew_service = make_curfew_services(player_ids=[1])
        seat(lobby_service, 1, LobbyKind.OPEN)
        _enable_curfew(player_repo, 1, curfew_hour=1, wake_hour=5)  # 1am-5am, not 11pm

        kicks = curfew_service.sweep([TEST_GUILD_ID], now=ELEVEN_PM_ET)

        assert kicks == []
        assert 1 in lobby_service.get_lobby(guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN).players

    def test_sweep_checks_every_lobby_kind(self):
        lobby_service, player_repo, curfew_service = make_curfew_services(player_ids=[1])
        seat(lobby_service, 1, LobbyKind.LOWSKILL)
        _enable_curfew(player_repo, 1)

        kicks = curfew_service.sweep([TEST_GUILD_ID], now=ELEVEN_PM_ET)

        assert [k.lobby_kind for k in kicks] == [LobbyKind.LOWSKILL]

    def test_sweep_with_no_lobby_members_is_a_noop(self):
        lobby_service, _, curfew_service = make_curfew_services(player_ids=[])

        assert curfew_service.sweep([TEST_GUILD_ID], now=ELEVEN_PM_ET) == []

    def test_sweep_scans_multiple_guilds(self):
        lobby_service, player_repo, curfew_service = make_curfew_services(player_ids=[1])
        seat(lobby_service, 1, LobbyKind.OPEN)
        _enable_curfew(player_repo, 1)

        kicks = curfew_service.sweep([TEST_GUILD_ID, 999999], now=ELEVEN_PM_ET)

        assert [k.guild_id for k in kicks] == [TEST_GUILD_ID]
