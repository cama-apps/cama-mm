"""Tests for LobbyService with pending match check."""

from datetime import datetime
from unittest.mock import MagicMock

import pytest

from domain.models.lobby import Lobby, LobbyKind
from domain.models.player import Player
from services.lobby_manager_service import LobbyManagerService
from services.lobby_service import LobbyService
from tests.fakes.lobby_repo import FakeLobbyRepo


@pytest.mark.parametrize(
    ("lobby_kind", "expected_amount"),
    [
        (LobbyKind.OPEN, 125),
        (LobbyKind.LOWSKILL, 275),
    ],
)
def test_lobby_embed_uses_preview_for_its_lobby_kind(lobby_kind, expected_amount):
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    loan_service = MagicMock()
    loan_service.get_first_game_pool_previews.return_value = {
        "open": 125,
        "lowskill": 275,
    }
    service = LobbyService(
        lobby_manager=lobby_manager,
        player_repo=MagicMock(),
        loan_service=loan_service,
    )
    lobby = Lobby(
        lobby_id=lobby_kind.lobby_id,
        created_by=None,
        created_at=datetime.now(),
        kind=lobby_kind,
    )

    embed = service.build_lobby_embed(lobby, guild_id=99)

    bonus_pool = next(field for field in embed.fields if field.name == "🎲 Bonus Pool")
    assert bonus_pool.value == (
        f"**{expected_amount} <:jopacoin:954159801049440297>** "
        "available if this lobby shuffles next."
    )


class TestLobbyServicePendingMatchCheck:
    """Test that join_lobby blocks players in pending matches."""

    def test_join_lobby_blocks_player_in_pending_match(self):
        """Player in pending match cannot join lobby."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = {
            "pending_match_id": 42,
            "shuffle_message_jump_url": "https://discord.com/jump/123",
        }

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "in_pending_match"
        assert pending_info["pending_match_id"] == 42
        assert pending_info["shuffle_message_jump_url"] == "https://discord.com/jump/123"
        lobby_manager.join_lobby.assert_not_called()

    def test_join_lobby_allows_player_not_in_pending_match(self):
        """Player not in pending match can join lobby."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        lobby_manager.get_or_create_lobby.return_value = lobby
        lobby_manager.join_lobby.return_value = "ok"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is True
        assert reason == ""
        assert pending_info == "ok"
        lobby_manager.join_lobby.assert_called_once()


def _player_with_rating(discord_id: int, rating: float) -> Player:
    return Player(
        name=f"Player {discord_id}",
        discord_id=discord_id,
        glicko_rating=rating,
    )


@pytest.mark.parametrize(
    ("rating", "expected_success", "expected_reason"),
    [
        (1399.99, True, ""),
        (1400.0, False, "rating_too_high"),
    ],
)
def test_lowskill_join_uses_strict_1400_cutoff(
    rating,
    expected_success,
    expected_reason,
):
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    player_repo = MagicMock()
    player_repo.get_by_id.return_value = _player_with_rating(1, rating)
    service = LobbyService(lobby_manager=lobby_manager, player_repo=player_repo)

    success, reason, pending_info = service.join_lobby(
        discord_id=1,
        guild_id=99,
        lobby_kind="lowskill",
    )

    assert success is expected_success
    assert reason == expected_reason
    if expected_success:
        assert pending_info == "ok"
    else:
        assert pending_info is None


def test_player_cannot_join_both_lobby_kinds():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    player_repo = MagicMock()
    player_repo.get_by_id.return_value = _player_with_rating(1, 1200)
    service = LobbyService(lobby_manager=lobby_manager, player_repo=player_repo)

    service.get_or_create_lobby(guild_id=99, lobby_kind="open")
    service.get_or_create_lobby(
        creator_id=1,
        guild_id=99,
        lobby_kind="lowskill",
    )
    assert service.join_lobby(1, guild_id=99, lobby_kind="open")[0] is True

    success, reason, pending_info = service.join_lobby(
        1,
        guild_id=99,
        lobby_kind="lowskill",
    )

    assert success is False
    assert reason == "in_other_lobby"
    assert pending_info is None
    assert service.get_lobby(guild_id=99, lobby_kind="lowskill").players == set()


def test_reserved_player_cannot_leave_or_switch_lobbies_until_released():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    player_repo = MagicMock()
    player_repo.get_by_id.return_value = _player_with_rating(1, 1200)
    service = LobbyService(lobby_manager=lobby_manager, player_repo=player_repo)

    service.get_or_create_lobby(guild_id=99, lobby_kind=LobbyKind.OPEN)
    service.get_or_create_lobby(
        creator_id=2,
        guild_id=99,
        lobby_kind=LobbyKind.LOWSKILL,
    )
    assert service.join_lobby(1, guild_id=99, lobby_kind=LobbyKind.OPEN)[0]
    assert service.reserve_lobby_players(
        [1],
        guild_id=99,
        lobby_kind=LobbyKind.OPEN,
    )

    assert service.leave_lobby(1, guild_id=99, lobby_kind=LobbyKind.OPEN) is False
    assert service.join_lobby(1, guild_id=99, lobby_kind=LobbyKind.LOWSKILL) == (
        False,
        "in_flight",
        None,
    )
    assert service.get_lobby_kind_for_player(1, guild_id=99) is LobbyKind.OPEN

    service.release_lobby_players(
        [1],
        guild_id=99,
        lobby_kind=LobbyKind.OPEN,
    )
    assert service.leave_lobby(1, guild_id=99, lobby_kind=LobbyKind.OPEN) is True
    assert service.join_lobby(1, guild_id=99, lobby_kind=LobbyKind.LOWSKILL)[0]


def test_same_lobby_players_cannot_be_reserved_by_two_match_starts():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    service = LobbyService(lobby_manager=lobby_manager, player_repo=MagicMock())
    lobby = service.get_or_create_lobby(guild_id=99, lobby_kind=LobbyKind.OPEN)
    lobby.players.update({1, 2})

    assert service.reserve_lobby_players([1, 2], 99, LobbyKind.OPEN) is True
    assert service.reserve_lobby_players([1, 2], 99, LobbyKind.OPEN) is False

    service.release_lobby_players([1, 2], 99, LobbyKind.OPEN)
    assert service.reserve_lobby_players([1, 2], 99, LobbyKind.OPEN) is True


def test_high_rated_player_can_view_but_not_create_lowskill_lobby():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    player_repo = MagicMock()
    player_repo.get_by_id.return_value = _player_with_rating(1, 1400)
    service = LobbyService(lobby_manager=lobby_manager, player_repo=player_repo)

    with pytest.raises(ValueError, match="rating_too_high"):
        service.get_or_create_lobby(
            creator_id=1,
            guild_id=99,
            lobby_kind="lowskill",
        )

    lobby_manager.get_or_create_lobby(
        creator_id=2,
        guild_id=99,
        lobby_kind="lowskill",
    )
    lobby = service.get_or_create_lobby(
        creator_id=1,
        guild_id=99,
        lobby_kind="lowskill",
    )
    assert lobby.created_by == 2


def test_lobby_metadata_and_readychecks_are_isolated_by_kind():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    service = LobbyService(lobby_manager=lobby_manager, player_repo=MagicMock())
    service.get_or_create_lobby(guild_id=99, lobby_kind="open")
    lobby_manager.get_or_create_lobby(guild_id=99, lobby_kind="lowskill")

    service.set_lobby_message_id(
        101,
        channel_id=201,
        guild_id=99,
        lobby_kind="open",
    )
    service.set_lobby_message_id(
        102,
        channel_id=202,
        guild_id=99,
        lobby_kind="lowskill",
    )
    service.set_readycheck_state(
        301,
        401,
        {1},
        {},
        guild_id=99,
        lobby_kind="open",
    )
    service.set_readycheck_state(
        302,
        402,
        {2},
        {},
        guild_id=99,
        lobby_kind="lowskill",
    )

    assert service.get_lobby_message_id(99, lobby_kind="open") == 101
    assert service.get_lobby_message_id(99, lobby_kind="lowskill") == 102
    assert service.get_readycheck_message_id(99, lobby_kind="open") == 301
    assert service.get_readycheck_message_id(99, lobby_kind="lowskill") == 302


def test_resetting_one_lobby_kind_leaves_the_other_open():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    service = LobbyService(lobby_manager=lobby_manager, player_repo=MagicMock())
    service.get_or_create_lobby(guild_id=99, lobby_kind="open")
    lobby_manager.get_or_create_lobby(guild_id=99, lobby_kind="lowskill")

    service.reset_lobby(guild_id=99, lobby_kind="lowskill")

    assert service.get_lobby(guild_id=99, lobby_kind="lowskill") is None
    assert service.get_lobby(guild_id=99, lobby_kind="open") is not None


class TestLobbyServiceJoinResults:

    def test_join_lobby_without_match_state_service_works(self):
        """LobbyService works without match_state_service (backwards compat)."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        lobby_manager.get_or_create_lobby.return_value = lobby
        lobby_manager.join_lobby.return_value = "ok"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=None,
        )

        success, _, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is True
        assert pending_info == "ok"

    def test_join_lobby_returns_lobby_full_reason(self):
        """Test that lobby full returns correct reason code.

        The capacity check lives inside the manager (under the state lock).
        LobbyService just maps the "full" reason string through.
        """
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None
        lobby_manager.join_lobby.return_value = "full"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
            max_players=12,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "lobby_full"
        assert pending_info is None

    def test_join_lobby_returns_already_joined_reason(self):
        """Test that already joined returns correct reason code."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None
        lobby_manager.join_lobby.return_value = "already_joined"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "already_joined"
        assert pending_info is None

    def test_join_lobby_conditional_returns_deprecated_reason(self):
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None
        lobby_manager.join_lobby_conditional.return_value = "deprecated"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby_conditional(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "conditional_join_deprecated"
        assert pending_info is None


def test_readycheck_confirmation_snapshot_is_available_through_lobby_service():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    lobby_manager.set_readycheck_state(111, 222, {1}, {})
    lobby_manager.add_readycheck_reaction(1, "<@1>")
    service = LobbyService(lobby_manager=lobby_manager, player_repo=MagicMock())

    assert service.get_readycheck_confirmation_snapshot() == (111, {1})


def test_lobby_readycheck_snapshot_loads_players_from_atomic_manager_snapshot():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    lobby = lobby_manager.get_or_create_lobby(creator_id=1)
    lobby.players.update({1, 2})
    lobby.player_join_times.update({1: 100.5, 2: 200.5})
    lobby_manager.set_readycheck_state(111, 222, {1, 2}, {})
    lobby_manager.add_readycheck_reaction(1, "<@1>")
    player_repo = MagicMock()
    players = [MagicMock(discord_id=1), MagicMock(discord_id=2)]
    player_repo.get_by_ids.return_value = players
    service = LobbyService(lobby_manager=lobby_manager, player_repo=player_repo)

    player_ids, loaded_players, join_times, readycheck = (
        service.get_lobby_players_and_readycheck_snapshot(guild_id=0)
    )

    assert set(player_ids) == {1, 2}
    assert loaded_players == players
    assert join_times == {1: 100.5, 2: 200.5}
    assert readycheck == (111, {1})
    player_repo.get_by_ids.assert_called_once_with(player_ids, 0)


def test_join_lobby_success_carries_commit_ordered_watermark():
    """A successful join returns joined_at_ns taken at the in-memory add.

    The one-shot lobby-watch claim cuts off at this watermark; a caller-side
    timestamp taken before the join call would silently exclude subscriptions
    committed while the join was in flight.
    """
    import time

    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    player_repo = MagicMock()
    player_repo.get_by_ids.return_value = []
    service = LobbyService(lobby_manager=lobby_manager, player_repo=player_repo)

    before_ns = time.time_ns()
    success, reason, context = service.join_lobby(1, 0)
    after_ns = time.time_ns()

    assert success is True
    assert reason == ""
    assert context == "ok"
    assert before_ns <= context.joined_at_ns <= after_ns

    # Re-joining does not fabricate a fresh watermark for a non-join.
    success, _, context = service.join_lobby(1, 0)
    assert success is False
    assert getattr(context, "joined_at_ns", None) is None


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
