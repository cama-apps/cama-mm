"""
Tests for PlayerQueue.
"""

from player_queue import PlayerQueue


def test_add_and_prevent_duplicates():
    queue = PlayerQueue()

    assert queue.add_player(101) is True
    assert queue.add_player(101) is False

    assert queue.size() == 1
    assert queue.is_in_queue(101) is True


def test_remove_updates_queue_and_set():
    queue = PlayerQueue()
    for pid in [1, 2, 3]:
        queue.add_player(pid)

    assert queue.remove_player(2) is True
    assert queue.remove_player(999) is False

    assert queue.get_all() == [1, 3]
    assert queue.is_in_queue(2) is False


def test_get_players_respects_order_and_removes():
    queue = PlayerQueue()
    for pid in [10, 20, 30]:
        queue.add_player(pid)

    assert queue.peek(2) == [10, 20]
    assert queue.size() == 3

    players = queue.get_players(2)
    assert players == [10, 20]
    assert queue.get_all() == [30]
    assert queue.size() == 1


def test_clear_empties_queue():
    queue = PlayerQueue()
    queue.add_player(1)
    queue.add_player(2)

    queue.clear()

    assert queue.size() == 0
    assert queue.get_all() == []
    assert queue.is_in_queue(1) is False
