"""Tests for lobby reaction helper behavior."""

from datetime import datetime

from domain.models.lobby import Lobby


def _lobby(
    regular_ids: list[int] | None = None,
    conditional_ids: list[int] | None = None,
) -> Lobby:
    lobby = Lobby(lobby_id=1, created_by=1, created_at=datetime.now())
    for player_id in regular_ids or []:
        lobby.add_player(player_id)
    for player_id in conditional_ids or []:
        lobby.add_conditional_player(player_id)
    return lobby


def test_conditional_click_is_forced_regular_for_tenth_projected_player():
    import bot

    lobby = _lobby(regular_ids=list(range(1, 10)))

    assert bot._should_force_regular_join_for_conditional_click(
        lobby,
        user_id=99,
        ready_threshold=10,
    )


def test_conditional_click_is_allowed_for_eleventh_projected_player():
    import bot

    lobby = _lobby(regular_ids=list(range(1, 11)))

    assert not bot._should_force_regular_join_for_conditional_click(
        lobby,
        user_id=99,
        ready_threshold=10,
    )


def test_existing_regular_player_at_threshold_stays_regular():
    import bot

    lobby = _lobby(regular_ids=list(range(1, 11)))

    assert bot._should_force_regular_join_for_conditional_click(
        lobby,
        user_id=10,
        ready_threshold=10,
    )


def test_existing_conditional_player_at_threshold_is_forced_regular():
    import bot

    lobby = _lobby(regular_ids=list(range(1, 10)), conditional_ids=[99])

    assert bot._should_force_regular_join_for_conditional_click(
        lobby,
        user_id=99,
        ready_threshold=10,
    )


def test_existing_conditional_player_above_threshold_can_remain_conditional():
    import bot

    lobby = _lobby(regular_ids=list(range(1, 11)), conditional_ids=[99])

    assert not bot._should_force_regular_join_for_conditional_click(
        lobby,
        user_id=99,
        ready_threshold=10,
    )


class _FakeUser:
    def __init__(self, user_id: int):
        self.id = user_id


class _FakeReaction:
    def __init__(self, emoji: str, user_ids: list[int]):
        self.emoji = emoji
        self._user_ids = list(user_ids)

    async def users(self):
        for user_id in self._user_ids:
            yield _FakeUser(user_id)


class _FakeMessage:
    def __init__(self, reactions: list[_FakeReaction]):
        self.reactions = reactions


class _FakeLobbyService:
    """Leave paths backed by a real Lobby object."""

    def __init__(self, lobby: Lobby):
        self._lobby = lobby

    def leave_lobby(self, user_id: int, guild_id) -> bool:
        return self._lobby.remove_player(user_id)

    def leave_lobby_conditional(self, user_id: int, guild_id) -> bool:
        return self._lobby.remove_conditional_player(user_id)


async def test_frogling_removal_leaves_forced_regular_player():
    """A force-joined regular whose only reaction is the frogling can leave.

    The frogling click force-joined them as REGULAR, so a conditional leave
    finds nothing; the handler must fall back to a regular leave.
    """
    import bot

    lobby = _lobby(regular_ids=[1, 2, 99])
    service = _FakeLobbyService(lobby)
    message = _FakeMessage([_FakeReaction("⚔️", [1, 2])])  # 99 holds no sword

    left = await bot._leave_lobby_for_frogling_removal(service, message, lobby, 99, 0)

    assert left is True
    assert 99 not in lobby.players


async def test_frogling_removal_ignored_when_user_still_holds_sword():
    """Bot cleanup of a stale frogling after a sword join must not undo it."""
    import bot

    lobby = _lobby(regular_ids=[99])
    service = _FakeLobbyService(lobby)
    message = _FakeMessage([_FakeReaction("⚔️", [99])])

    left = await bot._leave_lobby_for_frogling_removal(service, message, lobby, 99, 0)

    assert left is False
    assert 99 in lobby.players


async def test_frogling_removal_leaves_conditional_player():
    """The normal conditional leave path is unchanged."""
    import bot

    lobby = _lobby(regular_ids=[1], conditional_ids=[99])
    service = _FakeLobbyService(lobby)
    message = _FakeMessage([])

    left = await bot._leave_lobby_for_frogling_removal(service, message, lobby, 99, 0)

    assert left is True
    assert 99 not in lobby.conditional_players
    assert 1 in lobby.players


async def test_frogling_removal_noop_for_user_not_in_lobby():
    import bot

    lobby = _lobby(regular_ids=[1])
    service = _FakeLobbyService(lobby)
    message = _FakeMessage([])

    left = await bot._leave_lobby_for_frogling_removal(service, message, lobby, 99, 0)

    assert left is False
    assert lobby.players == {1}
