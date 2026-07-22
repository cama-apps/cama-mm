"""Focused coverage for player-scoped dig action history queries."""

import sqlite3
import time

import pytest

from repositories.dig_repository import DigRepository
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


def _insert_action(
    db_path: str,
    *,
    actor_id: int,
    target_id: int | None,
    action_type: str,
    created_at: int,
    guild_id: int = TEST_GUILD_ID,
) -> int:
    conn = sqlite3.connect(db_path)
    try:
        cursor = conn.execute(
            """
            INSERT INTO dig_actions (
                guild_id, actor_id, target_id, action_type,
                depth_before, depth_after, jc_delta, created_at
            )
            VALUES (?, ?, ?, ?, 0, 0, 0, ?)
            """,
            (guild_id, actor_id, target_id, action_type, created_at),
        )
        conn.commit()
        return cursor.lastrowid
    finally:
        conn.close()


def test_player_histories_merge_actor_and_target_without_self_duplicates(
    dig_repo, repo_db_path
):
    player_id = 101
    actor_row = _insert_action(
        repo_db_path,
        actor_id=player_id,
        target_id=202,
        action_type="dig",
        created_at=100,
    )
    target_row = _insert_action(
        repo_db_path,
        actor_id=202,
        target_id=player_id,
        action_type="help",
        created_at=200,
    )
    self_row = _insert_action(
        repo_db_path,
        actor_id=player_id,
        target_id=player_id,
        action_type="cheer",
        created_at=300,
    )
    _insert_action(
        repo_db_path,
        actor_id=303,
        target_id=404,
        action_type="sabotage",
        created_at=400,
    )
    _insert_action(
        repo_db_path,
        actor_id=player_id,
        target_id=202,
        action_type="dig",
        created_at=500,
        guild_id=TEST_GUILD_ID_SECONDARY,
    )

    all_recent = dig_repo.get_recent_actions(player_id, TEST_GUILD_ID, limit=10)
    assert [row["id"] for row in all_recent] == [self_row, target_row, actor_row]
    assert len({row["id"] for row in all_recent}) == 3

    recent = dig_repo.get_recent_actions(player_id, TEST_GUILD_ID, limit=2)
    assert [row["id"] for row in recent] == [self_row, target_row]
    assert len({row["id"] for row in recent}) == 2

    filtered = dig_repo.get_recent_actions(
        player_id,
        TEST_GUILD_ID,
        limit=10,
        action_type="help",
    )
    assert [row["id"] for row in filtered] == [target_row]

    jc_events = dig_repo.get_player_jc_events(player_id, TEST_GUILD_ID)
    assert [row["created_at"] for row in jc_events] == [100, 200, 300]
    assert [row["action_type"] for row in jc_events] == ["dig", "help", "cheer"]
    assert sum(
        row["actor_id"] == player_id and row["target_id"] == player_id
        for row in jc_events
    ) == 1


def test_recent_social_actions_applies_one_global_order_and_limit(
    dig_repo, repo_db_path
):
    player_id = 501
    now = int(time.time())
    expected: list[tuple[int, int]] = []
    action_types = ("sabotage", "help", "cheer")

    for sequence in range(24):
        created_at = now - 100 + sequence
        if sequence % 2:
            actor_id, target_id = 600 + sequence, player_id
        else:
            actor_id, target_id = player_id, 600 + sequence
        action_id = _insert_action(
            repo_db_path,
            actor_id=actor_id,
            target_id=target_id,
            action_type=action_types[sequence % len(action_types)],
            created_at=created_at,
        )
        expected.append((created_at, action_id))

    self_row = _insert_action(
        repo_db_path,
        actor_id=player_id,
        target_id=player_id,
        action_type="cheer",
        created_at=now,
    )
    expected.append((now, self_row))

    # Newer non-social, expired social, unrelated-player, and other-guild rows
    # must not displace an eligible action from the global top 20.
    _insert_action(
        repo_db_path,
        actor_id=player_id,
        target_id=700,
        action_type="dig",
        created_at=now + 1,
    )
    _insert_action(
        repo_db_path,
        actor_id=player_id,
        target_id=701,
        action_type="help",
        created_at=now - (49 * 3600),
    )
    _insert_action(
        repo_db_path,
        actor_id=702,
        target_id=703,
        action_type="help",
        created_at=now + 2,
    )
    _insert_action(
        repo_db_path,
        actor_id=player_id,
        target_id=704,
        action_type="help",
        created_at=now + 3,
        guild_id=TEST_GUILD_ID_SECONDARY,
    )

    actions = dig_repo.get_recent_social_actions(player_id, TEST_GUILD_ID)
    expected_ids = [
        action_id
        for _created_at, action_id in sorted(expected, reverse=True)[:20]
    ]

    assert [row["id"] for row in actions] == expected_ids
    assert len(actions) == 20
    assert len({row["id"] for row in actions}) == 20
