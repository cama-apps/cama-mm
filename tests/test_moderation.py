"""Focused persistence and policy tests for lobby moderation."""

import sqlite3

import pytest

from domain.models.moderation import (
    ActiveSuspensionExistsError,
    ModerationEventType,
    SuspensionCompletion,
    SuspensionScope,
)
from infrastructure.schema_manager import SchemaManager
from repositories.match_repository import MatchRepository
from repositories.moderation_repository import ModerationRepository
from services.moderation_service import (
    ModerationService,
    parse_duration_seconds,
    parse_expiry,
)
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

NOW = 2_000_000_000


@pytest.fixture
def moderation(repo_db_path):
    repo = ModerationRepository(repo_db_path)
    return ModerationService(repo), repo


def _save_pending(db_path: str, guild_id: int) -> int:
    return MatchRepository(db_path).save_pending_match(guild_id, {})


def _complete_match(
    db_path: str,
    guild_id: int,
    pending_match_id: int,
    lobby_kind: str,
) -> int:
    return MatchRepository(db_path).record_match_core_atomic(
        team1_ids=[],
        team2_ids=[],
        winning_team=1,
        guild_id=guild_id,
        dotabuff_match_id=None,
        lobby_type="shuffle",
        lobby_kind=lobby_kind,
        balancing_rating_system="glicko",
        winning_ids=[],
        losing_ids=[],
        glicko_updates=[],
        openskill_updates=[],
        rating_history_rows=[],
        match_prediction={
            "radiant_rating": 1500.0,
            "dire_rating": 1500.0,
            "radiant_rd": 200.0,
            "dire_rd": 200.0,
            "expected_radiant_win_prob": 0.5,
        },
        last_match_date_iso="2026-08-05T00:00:00+00:00",
        first_calibration_ids=[],
        first_calibration_unix=0,
        effective_avoid_ids=[],
        effective_deal_ids=[],
        pending_match_id=pending_match_id,
    )


def test_schema_creates_private_moderation_tables_and_match_kind(repo_db_path):
    with sqlite3.connect(repo_db_path) as conn:
        tables = {
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }
        match_columns = {
            row[1] for row in conn.execute("PRAGMA table_info(matches)")
        }
        suspension_columns = {
            row[1] for row in conn.execute("PRAGMA table_info(lobby_suspensions)")
        }
        event_columns = {
            row[1] for row in conn.execute("PRAGMA table_info(moderation_events)")
        }

    assert {"lobby_suspensions", "moderation_events"} <= tables
    assert "lobby_kind" in match_columns
    assert {
        "scope",
        "completion",
        "expires_at",
        "matches_required",
        "matches_remaining",
        "pending_match_watermark",
        "revision",
        "active",
    } <= suspension_columns
    assert {
        "matches_remaining",
        "wins_required",
        "wins_remaining",
        "match_id",
    } <= event_columns

    with sqlite3.connect(repo_db_path) as conn:
        lowprio_columns = {
            row[1] for row in conn.execute("PRAGMA table_info(low_priority_state)")
        }
    assert {"wins_required", "start_pending_match_id"} <= lowprio_columns


def test_low_priority_rebuild_preserves_legacy_progress(temp_db_path):
    conn = sqlite3.connect(temp_db_path)
    conn.row_factory = sqlite3.Row
    try:
        conn.execute(
            """
            CREATE TABLE low_priority_state (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                wins_remaining INTEGER NOT NULL DEFAULT 3,
                active INTEGER NOT NULL DEFAULT 1,
                reason TEXT,
                set_by INTEGER NOT NULL,
                removed_by INTEGER,
                removed_reason TEXT,
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )
        conn.execute(
            """
            INSERT INTO low_priority_state (
                discord_id, guild_id, wins_remaining, active, reason, set_by
            )
            VALUES (101, 77, 2, 1, 'legacy', 900)
            """
        )
        manager = SchemaManager(temp_db_path)
        manager._migration_expand_low_priority_state(conn.cursor())
        row = conn.execute(
            "SELECT * FROM low_priority_state WHERE discord_id = 101 AND guild_id = 77"
        ).fetchone()
    finally:
        conn.close()

    assert row["wins_required"] == 3
    assert row["wins_remaining"] == 2
    assert row["start_pending_match_id"] is None
    assert row["reason"] == "legacy"


@pytest.mark.parametrize(
    ("text", "seconds"),
    [
        ("5m", 300),
        ("30m", 1_800),
        ("2h", 7_200),
        ("3d", 259_200),
        ("1w", 604_800),
        ("1d12h", 129_600),
        ("1d 12h 30m", 131_400),
        ("365d", 31_536_000),
    ],
)
def test_duration_parsing(text, seconds):
    assert parse_duration_seconds(text) == seconds
    assert parse_expiry(text, now=NOW) == NOW + seconds
    assert ModerationService.parse_expiry(text, now=NOW) == NOW + seconds


@pytest.mark.parametrize(
    "text",
    ["", "2", "tomorrow", "1x", "0m", "299s", "366d", "1h nope"],
)
def test_duration_parsing_rejects_ambiguous_or_invalid_text(text):
    with pytest.raises(ValueError):
        parse_duration_seconds(text)


@pytest.mark.parametrize(
    ("completion", "expires_at", "matches", "message"),
    [
        ("time", NOW + 299, None, "5 minutes and 365 days"),
        ("time", NOW + 31_536_001, None, "5 minutes and 365 days"),
        ("matches", None, 0, "between 1 and 20"),
        ("matches", None, 21, "between 1 and 20"),
    ],
)
def test_suspension_plan_bounds_are_enforced(
    moderation,
    completion,
    expires_at,
    matches,
    message,
):
    service, _repo = moderation

    with pytest.raises(ValueError, match=message):
        service.create_suspension(
            99,
            TEST_GUILD_ID,
            actor_id=900,
            reason="Out of bounds",
            completion=completion,
            expires_at=expires_at,
            matches=matches,
            now=NOW,
        )


def test_time_suspension_completes_once_and_is_audited(moderation):
    service, _repo = moderation
    created = service.create_suspension(
        101,
        TEST_GUILD_ID,
        actor_id=900,
        reason="Repeated no-shows",
        expires_at=NOW + 300,
        now=NOW,
    )

    assert created.scope is SuspensionScope.ALL
    assert created.completion is SuspensionCompletion.TIME
    assert service.get_active_suspension(101, TEST_GUILD_ID, now=NOW + 299)
    assert service.get_active_suspension(101, TEST_GUILD_ID, now=NOW + 300) is None
    assert service.get_active_suspension(101, TEST_GUILD_ID, now=NOW + 301) is None

    history = service.get_history(TEST_GUILD_ID, 101)
    assert [event.event_type for event in history] == [
        ModerationEventType.COMPLETE,
        ModerationEventType.SUSPEND,
    ]


def test_match_suspension_captures_watermark_and_filters_scope(
    repo_db_path,
    moderation,
):
    service, _repo = moderation
    before = _save_pending(repo_db_path, TEST_GUILD_ID)
    created = service.create_suspension(
        102,
        TEST_GUILD_ID,
        actor_id=900,
        reason="Cooldown",
        scope="open",
        completion="matches",
        matches=1,
        now=NOW,
    )
    assert created.pending_match_watermark == before
    assert service.get_pending_match_watermark(TEST_GUILD_ID) == before

    # A match that was already pending when the suspension was issued is
    # grandfathered even when its lobby kind matches the suspension.
    _complete_match(
        repo_db_path,
        TEST_GUILD_ID,
        before,
        SuspensionScope.OPEN.value,
    )
    grandfathered = service.get_active_suspension(
        102,
        TEST_GUILD_ID,
        lobby_kind="open",
        now=NOW,
    )
    assert grandfathered is not None
    assert grandfathered.matches_remaining == 1

    lowskill_match = _save_pending(repo_db_path, TEST_GUILD_ID)
    _complete_match(
        repo_db_path,
        TEST_GUILD_ID,
        lowskill_match,
        SuspensionScope.LOWSKILL.value,
    )
    active = service.get_active_suspension(
        102,
        TEST_GUILD_ID,
        lobby_kind="open",
        now=NOW,
    )
    assert active is not None
    assert active.matches_completed == 0
    assert active.matches_remaining == 1
    assert (
        service.get_active_suspension(
            102,
            TEST_GUILD_ID,
            lobby_kind="lowskill",
            now=NOW,
        )
        is None
    )

    open_match = _save_pending(repo_db_path, TEST_GUILD_ID)
    recorded_match_id = _complete_match(
        repo_db_path,
        TEST_GUILD_ID,
        open_match,
        SuspensionScope.OPEN.value,
    )
    assert service.get_active_suspension(102, TEST_GUILD_ID, now=NOW) is None
    completion = service.get_history(TEST_GUILD_ID, 102)[0]
    assert completion.event_type is ModerationEventType.COMPLETE
    assert completion.match_id == recorded_match_id

    # Retrying the same pending match returns the original match and cannot
    # decrement or append another completion event.
    assert (
        _complete_match(
            repo_db_path,
            TEST_GUILD_ID,
            open_match,
            SuspensionScope.OPEN.value,
        )
        == recorded_match_id
    )
    assert sum(
        event.event_type is ModerationEventType.COMPLETE
        for event in service.get_history(TEST_GUILD_ID, 102)
    ) == 1


def test_either_and_both_completion_semantics(repo_db_path, moderation):
    service, _repo = moderation
    service.create_suspension(
        201,
        TEST_GUILD_ID,
        actor_id=900,
        reason="Either",
        completion="either",
        expires_at=NOW + 300,
        matches=2,
        now=NOW,
    )
    service.create_suspension(
        202,
        TEST_GUILD_ID,
        actor_id=900,
        reason="Both",
        completion="both",
        expires_at=NOW + 300,
        matches=1,
        now=NOW,
    )

    assert service.get_active_suspension(201, TEST_GUILD_ID, now=NOW + 300) is None
    assert service.get_active_suspension(202, TEST_GUILD_ID, now=NOW + 300)

    pending_id = _save_pending(repo_db_path, TEST_GUILD_ID)
    _complete_match(repo_db_path, TEST_GUILD_ID, pending_id, "open")
    assert service.get_active_suspension(202, TEST_GUILD_ID, now=NOW + 300) is None


def test_create_replace_lift_and_guild_isolation(moderation):
    service, _repo = moderation
    service.create_suspension(
        301,
        TEST_GUILD_ID,
        actor_id=900,
        reason="Initial",
        expires_at=NOW + 300,
        now=NOW,
    )
    with pytest.raises(ActiveSuspensionExistsError):
        service.create_suspension(
            301,
            TEST_GUILD_ID,
            actor_id=901,
            reason="Duplicate",
            expires_at=NOW + 400,
            now=NOW,
        )

    replaced = service.replace_suspension(
        301,
        TEST_GUILD_ID,
        actor_id=901,
        reason="Escalated",
        completion="matches",
        matches=3,
        now=NOW,
    )
    assert replaced.reason == "Escalated"
    assert replaced.matches_remaining == 3
    assert service.get_active_suspension(301, TEST_GUILD_ID_SECONDARY, now=NOW) is None

    lifted = service.lift_suspension(
        301,
        TEST_GUILD_ID,
        actor_id=902,
        reason="Appeal granted",
        now=NOW,
    )
    assert lifted is not None and lifted.active is False
    assert service.get_active_suspension(301, TEST_GUILD_ID, now=NOW) is None
    assert [event.event_type for event in service.get_history(TEST_GUILD_ID, 301)] == [
        ModerationEventType.LIFT,
        ModerationEventType.REPLACE,
        ModerationEventType.SUSPEND,
    ]


def test_stale_close_cannot_close_a_concurrent_replacement(moderation):
    service, repo = moderation
    original = service.create_suspension(
        302,
        TEST_GUILD_ID,
        actor_id=900,
        reason="Original",
        expires_at=NOW + 300,
        now=NOW,
    )
    replacement = service.replace_suspension(
        302,
        TEST_GUILD_ID,
        actor_id=901,
        reason="Replacement",
        expires_at=NOW + 600,
        now=NOW,
    )

    assert replacement.revision == original.revision + 1
    assert (
        repo.close_suspension(
            302,
            TEST_GUILD_ID,
            event_type="complete",
            source="system",
            actor_id=None,
            reason="Stale natural completion",
            expected_revision=original.revision,
            now=NOW + 300,
        )
        is None
    )
    current = service.get_active_suspension(302, TEST_GUILD_ID, now=NOW + 300)
    assert current is not None
    assert current.reason == "Replacement"
    assert current.revision == replacement.revision
    assert [event.event_type for event in service.get_history(TEST_GUILD_ID, 302)] == [
        ModerationEventType.REPLACE,
        ModerationEventType.SUSPEND,
    ]


def test_active_lookup_retries_and_returns_a_racing_replacement(
    moderation,
    monkeypatch,
):
    service, repo = moderation
    original = service.create_suspension(
        303,
        TEST_GUILD_ID,
        actor_id=900,
        reason="Expiring",
        expires_at=NOW + 300,
        now=NOW,
    )
    real_close = repo.close_suspension
    replaced = False

    def close_after_replacement(*args, **kwargs):
        nonlocal replaced
        if not replaced:
            replaced = True
            repo.save_suspension(
                303,
                TEST_GUILD_ID,
                scope="all",
                completion="time",
                expires_at=NOW + 600,
                matches_required=None,
                pending_match_watermark=None,
                reason="Concurrent replacement",
                source="admin",
                actor_id=901,
                replace=True,
                now=NOW + 300,
            )
        return real_close(*args, **kwargs)

    monkeypatch.setattr(repo, "close_suspension", close_after_replacement)

    active = service.get_active_suspension(303, TEST_GUILD_ID, now=NOW + 300)

    assert active is not None
    assert active.reason == "Concurrent replacement"
    assert active.revision == original.revision + 1
    assert [event.event_type for event in service.get_history(TEST_GUILD_ID, 303)] == [
        ModerationEventType.REPLACE,
        ModerationEventType.SUSPEND,
    ]


def test_list_history_kick_and_low_priority_audit_are_guild_scoped(
    moderation,
):
    service, _repo = moderation
    service.create_suspension(
        401,
        TEST_GUILD_ID,
        actor_id=900,
        reason="Lobby scope",
        scope="lowskill",
        expires_at=NOW + 300,
        now=NOW,
    )
    service.create_suspension(
        402,
        TEST_GUILD_ID_SECONDARY,
        actor_id=900,
        reason="Other guild",
        expires_at=NOW + 300,
        now=NOW,
    )
    service.record_kick(
        403,
        TEST_GUILD_ID,
        actor_id=901,
        lobby_kind="open",
        reason="Removed from current lobby",
        source="creator",
        now=NOW,
    )
    service.record_low_priority_event(
        404,
        TEST_GUILD_ID,
        event_type="lowprio_assign",
        wins_required=3,
        wins_remaining=3,
        actor_id=902,
        reason="Abandon",
        now=NOW,
    )
    service.record_low_priority_event(
        404,
        TEST_GUILD_ID,
        event_type="lowprio_complete",
        wins_required=3,
        wins_remaining=0,
        match_id=77,
        now=NOW + 1,
    )

    assert [item.discord_id for item in service.list_active_suspensions(TEST_GUILD_ID)] == [401]
    assert service.list_active_suspensions(TEST_GUILD_ID, lobby_kind="open") == []
    events = service.get_history(TEST_GUILD_ID)
    assert {event.discord_id for event in events} == {401, 403, 404}
    lowprio = service.get_history(TEST_GUILD_ID, 404)
    assert lowprio[0].event_type is ModerationEventType.LOWPRIO_COMPLETE
    assert lowprio[0].wins_remaining == 0
    assert lowprio[0].match_id == 77


def test_moderation_events_are_append_only(repo_db_path, moderation):
    service, _repo = moderation
    event = service.record_kick(
        501,
        TEST_GUILD_ID,
        actor_id=900,
        lobby_kind="open",
        now=NOW,
    )

    with sqlite3.connect(repo_db_path) as conn, pytest.raises(sqlite3.DatabaseError):
        conn.execute(
            "UPDATE moderation_events SET reason = 'rewritten' WHERE event_id = ?",
            (event.event_id,),
        )
    with sqlite3.connect(repo_db_path) as conn, pytest.raises(sqlite3.DatabaseError):
        conn.execute(
            "DELETE FROM moderation_events WHERE event_id = ?",
            (event.event_id,),
        )
