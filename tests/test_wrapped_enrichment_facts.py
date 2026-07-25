"""Integration tests for Wrapped's compact OpenDota fact projection."""

import json
import sqlite3
from datetime import UTC, datetime
from unittest.mock import Mock

import pytest

from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from repositories.wrapped_repository import WrappedRepository
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


def _fact_rows(db_path: str, match_id: int) -> list[tuple]:
    with sqlite3.connect(db_path) as conn:
        return conn.execute(
            """
            SELECT
                discord_id, actions_per_min, courier_kills, pings,
                rapier_count, lane_role, comeback, throw
            FROM wrapped_enrichment_facts
            WHERE guild_id = ? AND match_id = ?
            ORDER BY discord_id
            """,
            (TEST_GUILD_ID, match_id),
        ).fetchall()


def _apply_facts(
    repo: MatchRepository,
    match_id: int,
    facts: list[dict],
    *,
    source: str = "manual",
    valve_match_id: int = 8000000001,
) -> None:
    repo.apply_enrichment_atomic(
        match_id=match_id,
        valve_match_id=valve_match_id,
        duration_seconds=2400,
        radiant_score=30,
        dire_score=20,
        game_mode=2,
        enrichment_data=json.dumps({"players": []}),
        enrichment_source=source,
        enrichment_confidence=None,
        participant_updates=[],
        guild_id=TEST_GUILD_ID,
        wrapped_facts=facts,
    )


def test_atomic_write_wipe_and_reenrich_keep_compact_facts_consistent(repo_db_path):
    match_repo = MatchRepository(repo_db_path)
    wrapped_repo = WrappedRepository(repo_db_path)
    match_id = match_repo.record_match(
        [100],
        [200],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    initial = [
        {
            "discord_id": 100,
            "actions_per_min": 321,
            "courier_kills": 2,
            "pings": 44,
            "rapier_count": 2,
            "lane_role": 2,
            "comeback": 9000,
            "throw": 4000,
        },
        {
            "discord_id": 200,
            "actions_per_min": None,
            "courier_kills": None,
            "pings": None,
            "rapier_count": 0,
            "lane_role": None,
            "comeback": 9000,
            "throw": 4000,
        },
        {
            # Caller-supplied facts cannot create attribution for someone who
            # was not a participant in this guild's match.
            "discord_id": 999,
            "actions_per_min": 9999,
            "rapier_count": 9,
        },
    ]
    _apply_facts(match_repo, match_id, initial)

    assert _fact_rows(repo_db_path, match_id) == [
        (100, 321, 2, 44, 2, 2, 9000, 4000),
        (200, None, None, None, 0, None, 9000, 4000),
    ]
    year = datetime.now(UTC).year
    end_ts = int(datetime(year + 1, 1, 1, tzinfo=UTC).timestamp())
    player_rows = wrapped_repo.get_player_year_matches(
        100,
        TEST_GUILD_ID,
        year,
        end_ts,
    )
    assert len(player_rows) == 1
    assert "enrichment_data" not in player_rows[0]
    assert {
        key: player_rows[0][key]
        for key in (
            "wrapped_facts_match_id",
            "actions_per_min",
            "courier_kills",
            "pings",
            "rapier_count",
            "wrapped_lane_role",
            "comeback",
            "throw",
        )
    } == {
        "wrapped_facts_match_id": match_id,
        "actions_per_min": 321,
        "courier_kills": 2,
        "pings": 44,
        "rapier_count": 2,
        "wrapped_lane_role": 2,
        "comeback": 9000,
        "throw": 4000,
    }

    # A different guild cannot remove this guild's projection.
    assert (
        match_repo.wipe_match_enrichment(
            match_id,
            guild_id=TEST_GUILD_ID_SECONDARY,
        )
        is False
    )
    assert len(_fact_rows(repo_db_path, match_id)) == 2

    assert match_repo.wipe_match_enrichment(match_id, TEST_GUILD_ID) is True
    assert _fact_rows(repo_db_path, match_id) == []

    replacement = [
        {
            **fact,
            "actions_per_min": 999 if fact["discord_id"] == 100 else None,
            "comeback": 123,
            "throw": 456,
        }
        for fact in initial
    ]
    _apply_facts(
        match_repo,
        match_id,
        replacement,
        valve_match_id=8000000002,
    )
    assert _fact_rows(repo_db_path, match_id) == [
        (100, 999, 2, 44, 2, 2, 123, 456),
        (200, None, None, None, 0, None, 123, 456),
    ]


def test_bulk_wipes_remove_only_their_compact_fact_rows(repo_db_path):
    repo = MatchRepository(repo_db_path)
    auto_match_id = repo.record_match(
        [100],
        [],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    manual_match_id = repo.record_match(
        [200],
        [],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    auto_facts = [{"discord_id": 100, "rapier_count": 0}]
    manual_facts = [{"discord_id": 200, "rapier_count": 0}]
    _apply_facts(
        repo,
        auto_match_id,
        auto_facts,
        source="auto",
        valve_match_id=8000000010,
    )
    _apply_facts(
        repo,
        manual_match_id,
        manual_facts,
        valve_match_id=8000000011,
    )

    assert repo.wipe_auto_discovered_enrichments(TEST_GUILD_ID) == 1
    assert _fact_rows(repo_db_path, auto_match_id) == []
    assert len(_fact_rows(repo_db_path, manual_match_id)) == 1

    assert repo.wipe_all_enrichments(TEST_GUILD_ID) == 1
    assert _fact_rows(repo_db_path, manual_match_id) == []


@pytest.mark.parametrize("link_method", ["set", "add", "bulk"])
def test_steam_id_link_rebuilds_previously_unmatched_history(
    repo_db_path,
    link_method,
):
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(
        discord_id=100,
        discord_username="late-link",
        guild_id=TEST_GUILD_ID,
    )
    match_repo = MatchRepository(repo_db_path)
    match_id = match_repo.record_match(
        [100],
        [],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    payload = {
        "players": [
            {
                "account_id": 12345,
                "actions_per_min": 321,
                "courier_kills": 2,
                "pings": 44,
                "purchase_log": [{"key": "rapier"}, {"key": "rapier"}],
                "lane_role": 2,
            }
        ],
        "comeback": 111,
        "throw": 222,
    }
    match_repo.update_match_enrichment(
        match_id,
        valve_match_id=8000000030,
        duration_seconds=2400,
        radiant_score=30,
        dire_score=20,
        game_mode=2,
        enrichment_data=json.dumps(payload),
        enrichment_source="manual",
    )
    assert _fact_rows(repo_db_path, match_id) == [
        (100, None, None, None, 0, None, 111, 222)
    ]

    if link_method == "set":
        player_repo.set_steam_id(100, 12345)
    elif link_method == "add":
        player_repo.add_steam_id(100, 12345, is_primary=True)
    else:
        assert player_repo.add_steam_ids_bulk([(100, 12345)]) == [
            {"success": True, "is_primary": True}
        ]

    assert _fact_rows(repo_db_path, match_id) == [
        (100, 321, 2, 44, 2, 2, 111, 222)
    ]


def test_steam_id_unlink_removes_stale_historical_attribution(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(
        discord_id=100,
        discord_username="unlink",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_steam_id(100, 11111, is_primary=True)
    player_repo.add_steam_id(100, 12345)
    match_repo = MatchRepository(repo_db_path)
    match_id = match_repo.record_match(
        [100],
        [],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    match_repo.update_match_enrichment(
        match_id,
        valve_match_id=8000000031,
        duration_seconds=2400,
        radiant_score=30,
        dire_score=20,
        game_mode=2,
        enrichment_data=json.dumps(
            {
                "players": [
                    {
                        "account_id": 12345,
                        "actions_per_min": 444,
                        "purchase_log": [{"key": "rapier"}],
                    }
                ],
                "comeback": 333,
                "throw": 222,
            }
        ),
        enrichment_source="manual",
    )
    assert _fact_rows(repo_db_path, match_id)[0][1] == 444

    assert player_repo.remove_steam_id(100, 12345) is True
    assert _fact_rows(repo_db_path, match_id) == [
        (100, None, None, None, 0, None, 333, 222)
    ]


def test_steam_id_reassignment_moves_historical_attribution(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    for discord_id in (100, 200):
        player_repo.add(
            discord_id=discord_id,
            discord_username=f"player-{discord_id}",
            guild_id=TEST_GUILD_ID,
        )
    player_repo.add_steam_id(100, 12345, is_primary=True)
    match_repo = MatchRepository(repo_db_path)
    match_id = match_repo.record_match(
        [100],
        [200],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    match_repo.update_match_enrichment(
        match_id,
        valve_match_id=8000000032,
        duration_seconds=2400,
        radiant_score=30,
        dire_score=20,
        game_mode=2,
        enrichment_data=json.dumps(
            {
                "players": [
                    {
                        "account_id": 12345,
                        "actions_per_min": 555,
                        "lane_role": 3,
                    }
                ],
                "comeback": 50,
                "throw": 25,
            }
        ),
        enrichment_source="manual",
    )
    assert _fact_rows(repo_db_path, match_id) == [
        (100, 555, None, None, 0, 3, 50, 25),
        (200, None, None, None, 0, None, 50, 25),
    ]

    assert player_repo.remove_steam_id(100, 12345) is True
    player_repo.add_steam_id(200, 12345, is_primary=True)

    assert _fact_rows(repo_db_path, match_id) == [
        (100, None, None, None, 0, None, 50, 25),
        (200, 555, None, None, 0, 3, 50, 25),
    ]


def test_bulk_link_parses_a_shared_historical_match_once(
    repo_db_path,
    monkeypatch,
):
    player_repo = PlayerRepository(repo_db_path)
    for discord_id in (100, 200):
        player_repo.add(
            discord_id=discord_id,
            discord_username=f"bulk-{discord_id}",
            guild_id=TEST_GUILD_ID,
        )
    match_repo = MatchRepository(repo_db_path)
    match_id = match_repo.record_match(
        [100],
        [200],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    match_repo.update_match_enrichment(
        match_id,
        valve_match_id=8000000033,
        duration_seconds=2400,
        radiant_score=30,
        dire_score=20,
        game_mode=2,
        enrichment_data=json.dumps(
            {
                "players": [
                    {"account_id": 11111, "actions_per_min": 111},
                    {"account_id": 22222, "actions_per_min": 222},
                ]
            }
        ),
        enrichment_source="manual",
    )
    loads = Mock(wraps=json.loads)
    monkeypatch.setattr("repositories.player_repository.json.loads", loads)

    results = player_repo.add_steam_ids_bulk([
        (100, 11111),
        (200, 22222),
    ])

    assert all(result["success"] for result in results)
    assert loads.call_count == 1
    assert _fact_rows(repo_db_path, match_id)[0][1] == 111
    assert _fact_rows(repo_db_path, match_id)[1][1] == 222


@pytest.mark.parametrize(
    ("method_name", "source"),
    [
        ("wipe_auto_discovered_enrichments", "auto"),
        ("wipe_all_enrichments", "manual"),
    ],
)
def test_bulk_wipes_lock_before_selecting_match_ids(
    repo_db_path,
    monkeypatch,
    method_name,
    source,
):
    repo = MatchRepository(repo_db_path)
    match_id = repo.record_match(
        [100],
        [],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    _apply_facts(
        repo,
        match_id,
        [{"discord_id": 100, "rapier_count": 0}],
        source=source,
    )

    statements = []
    original_get_connection = repo.get_connection

    def traced_get_connection():
        conn = original_get_connection()
        conn.set_trace_callback(statements.append)
        return conn

    monkeypatch.setattr(repo, "get_connection", traced_get_connection)

    assert getattr(repo, method_name)(TEST_GUILD_ID) == 1

    normalized = [" ".join(statement.upper().split()) for statement in statements]
    begin_index = normalized.index("BEGIN IMMEDIATE")
    selection_index = next(
        index
        for index, statement in enumerate(normalized)
        if statement.startswith("SELECT MATCH_ID FROM MATCHES")
    )
    assert normalized.count("BEGIN IMMEDIATE") == 1
    assert begin_index < selection_index


def test_legacy_enrichment_write_derives_compact_facts(repo_db_path):
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            """
            INSERT INTO players (
                discord_id, guild_id, discord_username, steam_id
            ) VALUES (?, ?, ?, ?)
            """,
            (100, TEST_GUILD_ID, "legacy", 12345),
        )

    repo = MatchRepository(repo_db_path)
    match_id = repo.record_match(
        [100],
        [],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    payload = {
        "players": [
            {
                "account_id": 12345,
                "actions_per_min": 222,
                "courier_kills": 1,
                "pings": 7,
                "purchase_log": [{"key": "rapier"}],
                "lane_role": 3,
            }
        ],
        "comeback": 111,
        "throw": 222,
    }

    repo.update_match_enrichment(
        match_id,
        valve_match_id=8000000020,
        duration_seconds=2400,
        radiant_score=30,
        dire_score=20,
        game_mode=2,
        enrichment_data=json.dumps(payload),
        enrichment_source="manual",
    )

    assert _fact_rows(repo_db_path, match_id) == [
        (100, 222, 1, 7, 1, 3, 111, 222)
    ]
