"""Regression coverage for Scout's normalized OpenDota ban projection."""

import json
import sqlite3

from infrastructure.schema_manager import SchemaManager
from repositories.match_repository import MatchRepository
from tests.conftest import TEST_GUILD_ID
from utils.match_bans import extract_match_bans


def _enrichment(*entries: dict) -> str:
    return json.dumps({"picks_bans": list(entries)})


def _record_match(repo: MatchRepository, *, radiant: int = 101, dire: int = 202) -> int:
    return repo.record_match(
        team1_ids=[radiant],
        team2_ids=[dire],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )


def _update_enrichment(
    repo: MatchRepository,
    match_id: int,
    payload: str,
    *,
    source: str | None = None,
) -> None:
    repo.update_match_enrichment(
        match_id=match_id,
        valve_match_id=8_181_518_332,
        duration_seconds=2400,
        radiant_score=35,
        dire_score=22,
        game_mode=2,
        enrichment_data=payload,
        enrichment_source=source,
    )


def test_extract_match_bans_keeps_only_valid_bans():
    payload = {
        "picks_bans": [
            {"is_pick": False, "team": 1, "hero_id": 10},
            {"is_pick": True, "team": 0, "hero_id": 20},
            {"is_pick": False, "team": 3, "hero_id": 30},
            {"is_pick": False, "team": 0, "hero_id": 0},
            "corrupt",
            {"is_pick": 0, "team": "0", "hero_id": "40"},
        ]
    }

    assert extract_match_bans(payload) == [
        (0, 1, 10),
        (5, 0, 40),
    ]
    assert extract_match_bans("{not-json") == []


def test_reenrichment_replaces_normalized_bans(match_repository):
    match_id = _record_match(match_repository)
    _update_enrichment(
        match_repository,
        match_id,
        _enrichment(
            {"is_pick": False, "team": 1, "hero_id": 10},
            {"is_pick": False, "team": 1, "hero_id": 20},
        ),
    )
    assert match_repository.get_bans_for_players([101], TEST_GUILD_ID) == {
        10: 1,
        20: 1,
    }

    _update_enrichment(
        match_repository,
        match_id,
        _enrichment(
            {"is_pick": False, "team": 0, "hero_id": 30},
            {"is_pick": False, "team": 1, "hero_id": 40},
        ),
    )

    assert match_repository.get_bans_for_players([101], TEST_GUILD_ID) == {40: 1}
    with match_repository.connection() as conn:
        rows = conn.execute(
            """
            SELECT team, hero_id
            FROM match_bans
            WHERE match_id = ?
            ORDER BY ban_index
            """,
            (match_id,),
        ).fetchall()
    assert [(row["team"], row["hero_id"]) for row in rows] == [(0, 30), (1, 40)]


def test_atomic_enrichment_projects_bans(match_repository):
    match_id = _record_match(match_repository)
    rowcount = match_repository.apply_enrichment_atomic(
        match_id=match_id,
        valve_match_id=8_181_518_332,
        duration_seconds=2400,
        radiant_score=35,
        dire_score=22,
        game_mode=2,
        enrichment_data=_enrichment(
            {"is_pick": False, "team": 1, "hero_id": 10},
        ),
        enrichment_source="auto",
        enrichment_confidence=1.0,
        participant_updates=[],
    )

    assert rowcount == 0
    assert match_repository.get_bans_for_players([101], TEST_GUILD_ID) == {10: 1}


def test_scout_ban_query_never_reads_raw_enrichment(
    match_repository,
    monkeypatch,
):
    match_id = _record_match(match_repository)
    _update_enrichment(
        match_repository,
        match_id,
        _enrichment(
            {"is_pick": False, "team": 1, "hero_id": 10},
        ),
    )

    real_get_connection = match_repository.get_connection

    def guarded_connection():
        conn = real_get_connection()

        def authorizer(action, table, column, _database, _trigger):
            if (
                action == sqlite3.SQLITE_READ
                and table == "matches"
                and column == "enrichment_data"
            ):
                return sqlite3.SQLITE_DENY
            return sqlite3.SQLITE_OK

        conn.set_authorizer(authorizer)
        return conn

    monkeypatch.setattr(match_repository, "get_connection", guarded_connection)

    assert match_repository.get_bans_for_players([101], TEST_GUILD_ID) == {10: 1}


def test_scout_ban_migration_backfills_existing_payloads(tmp_path):
    db_path = str(tmp_path / "legacy-scout.db")
    SchemaManager(db_path).initialize()

    with sqlite3.connect(db_path) as conn:
        cursor = conn.execute(
            """
            INSERT INTO matches (
                guild_id, team1_players, team2_players, winning_team,
                enrichment_data
            )
            VALUES (?, ?, ?, ?, ?)
            """,
            (
                TEST_GUILD_ID,
                "[101]",
                "[202]",
                1,
                _enrichment(
                    {"is_pick": False, "team": 1, "hero_id": 10},
                ),
            ),
        )
        match_id = cursor.lastrowid
        conn.execute(
            """
            INSERT INTO match_participants (
                match_id, discord_id, team_number, won, side, guild_id
            )
            VALUES (?, ?, 1, 1, 'radiant', ?)
            """,
            (match_id, 101, TEST_GUILD_ID),
        )
        conn.execute(
            """
            DELETE FROM schema_migrations
            WHERE name = 'create_match_bans_for_scout'
            """
        )
        conn.execute("DROP TABLE match_bans")
        conn.execute("DROP INDEX IF EXISTS idx_match_participants_scout")

    SchemaManager(db_path).initialize()
    repo = MatchRepository(db_path)

    assert repo.get_bans_for_players([101], TEST_GUILD_ID) == {10: 1}


def test_enrichment_wipes_delete_normalized_bans(match_repository):
    auto_match = _record_match(match_repository, radiant=101, dire=202)
    manual_match = _record_match(match_repository, radiant=101, dire=303)
    _update_enrichment(
        match_repository,
        auto_match,
        _enrichment({"is_pick": False, "team": 1, "hero_id": 10}),
        source="auto",
    )
    _update_enrichment(
        match_repository,
        manual_match,
        _enrichment({"is_pick": False, "team": 1, "hero_id": 20}),
        source="manual",
    )

    assert match_repository.wipe_auto_discovered_enrichments(TEST_GUILD_ID) == 1
    assert match_repository.get_bans_for_players([101], TEST_GUILD_ID) == {20: 1}

    assert match_repository.wipe_all_enrichments(TEST_GUILD_ID) == 1
    assert match_repository.get_bans_for_players([101], TEST_GUILD_ID) == {}


def test_single_enrichment_wipe_deletes_normalized_bans(match_repository):
    match_id = _record_match(match_repository)
    _update_enrichment(
        match_repository,
        match_id,
        _enrichment({"is_pick": False, "team": 1, "hero_id": 10}),
    )

    assert match_repository.wipe_match_enrichment(match_id, TEST_GUILD_ID)
    assert match_repository.get_bans_for_players([101], TEST_GUILD_ID) == {}
