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
