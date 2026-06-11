"""Tests for MafiaRepository."""

from __future__ import annotations

import sqlite3
import time

import pytest

from domain.models.mafia import (
    MafiaActionType,
    MafiaPhase,
    MafiaPlayer,
    MafiaRole,
    MafiaTwist,
    MafiaWinner,
)
from repositories.mafia_repository import MafiaRepository
from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


@pytest.fixture
def mafia_repo(repo_db_path):
    return MafiaRepository(repo_db_path)


def _seed_registered_player(
    repo, *, discord_id: int, guild_id: int, balance: int = 100
) -> None:
    player_repo = PlayerRepository(repo.db_path)
    try:
        player_repo.add(
            discord_id=discord_id,
            discord_username=f"user_{discord_id}",
            guild_id=guild_id,
        )
    except ValueError:
        pass
    player_repo.update_balance(discord_id, guild_id, balance)


def _seed_wheel_spin(
    repo, *, discord_id: int, guild_id: int, ts: int, registered: bool = True
) -> None:
    if registered:
        _seed_registered_player(repo, discord_id=discord_id, guild_id=guild_id)
    with repo.connection() as conn:
        conn.cursor().execute(
            """
            INSERT INTO wheel_spins (guild_id, discord_id, result, spin_time, is_bankrupt, is_golden)
            VALUES (?, ?, 0, ?, 0, 0)
            """,
            (guild_id, discord_id, ts),
        )


def _seed_dig_action(
    repo, *, actor_id: int, guild_id: int, ts: int, registered: bool = True
) -> None:
    if registered:
        _seed_registered_player(repo, discord_id=actor_id, guild_id=guild_id)
    with repo.connection() as conn:
        conn.cursor().execute(
            """
            INSERT INTO dig_actions (guild_id, actor_id, target_id, action_type,
                depth_before, depth_after, jc_delta, detail, created_at)
            VALUES (?, ?, NULL, 'dig', 0, 0, 0, NULL, ?)
            """,
            (guild_id, actor_id, ts),
        )


# ── Game CRUD ─────────────────────────────────────────────────────────────


def test_create_and_fetch_game(mafia_repo):
    gid = mafia_repo.create_game(
        TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 1000, 8, MafiaTwist.BLOOD_MOON
    )
    game = mafia_repo.get_game_by_id(gid)
    assert game is not None
    assert game.phase == MafiaPhase.NIGHT
    assert game.twist_event == MafiaTwist.BLOOD_MOON
    assert game.roster_size == 8

    same = mafia_repo.get_game_for_date(TEST_GUILD_ID, "2026-04-24")
    assert same is not None
    assert same.game_id == gid


def test_unique_game_per_date(mafia_repo):
    mafia_repo.create_game(TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 1000, 5, None)
    with pytest.raises(sqlite3.IntegrityError):
        mafia_repo.create_game(TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 2000, 5, None)


def test_get_active_game_excludes_resolved(mafia_repo):
    gid1 = mafia_repo.create_game(
        TEST_GUILD_ID, "2026-04-23", MafiaPhase.RESOLVED, 0, 5, None
    )
    mafia_repo.finalize_game(gid1, MafiaWinner.TOWN, 50, mvp_id=None)

    gid2 = mafia_repo.create_game(
        TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 100, 5, None
    )

    active = mafia_repo.get_active_game(TEST_GUILD_ID)
    assert active is not None
    assert active.game_id == gid2


def test_set_phase_and_thread_ids(mafia_repo):
    gid = mafia_repo.create_game(TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 0, 5, None)
    mafia_repo.set_phase(gid, MafiaPhase.DAY, night_ended_at=999)
    mafia_repo.set_thread_ids(
        gid, mafia_thread_id=111, discussion_thread_id=222, setup_message_id=333
    )
    game = mafia_repo.get_game_by_id(gid)
    assert game.phase == MafiaPhase.DAY
    assert game.night_ended_at == 999
    assert game.mafia_thread_id == 111
    assert game.discussion_thread_id == 222
    assert game.setup_message_id == 333


# ── Players ───────────────────────────────────────────────────────────────


def test_add_and_get_players(mafia_repo):
    gid = mafia_repo.create_game(TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 0, 5, None)
    players = [
        MafiaPlayer(
            game_id=gid,
            discord_id=1001,
            guild_id=TEST_GUILD_ID,
            role=MafiaRole.MAFIA,
            is_godfather=True,
            hero_name="Pudge",
        ),
        MafiaPlayer(
            game_id=gid,
            discord_id=1002,
            guild_id=TEST_GUILD_ID,
            role=MafiaRole.TOWNIE,
            hero_name="CM",
        ),
    ]
    mafia_repo.add_players(gid, players)

    fetched = mafia_repo.get_players(gid)
    assert len(fetched) == 2
    by_id = {p.discord_id: p for p in fetched}
    assert by_id[1001].is_godfather is True
    assert by_id[1001].role == MafiaRole.MAFIA
    assert by_id[1002].hero_name == "CM"


def test_get_alive_players_filters(mafia_repo):
    gid = mafia_repo.create_game(TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 0, 3, None)
    mafia_repo.add_players(
        gid,
        [
            MafiaPlayer(gid, 1, TEST_GUILD_ID, MafiaRole.MAFIA),
            MafiaPlayer(gid, 2, TEST_GUILD_ID, MafiaRole.TOWNIE),
            MafiaPlayer(gid, 3, TEST_GUILD_ID, MafiaRole.TOWNIE),
        ],
    )
    mafia_repo.set_player_alive(gid, 2, False, eliminated_phase=MafiaPhase.NIGHT)

    alive = mafia_repo.get_alive_players(gid)
    assert {p.discord_id for p in alive} == {1, 3}

    alive_townies = mafia_repo.get_alive_players(gid, role=MafiaRole.TOWNIE)
    assert {p.discord_id for p in alive_townies} == {3}


# ── Actions ───────────────────────────────────────────────────────────────


def test_record_action_upsert(mafia_repo):
    gid = mafia_repo.create_game(TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 0, 3, None)
    mafia_repo.add_players(
        gid,
        [
            MafiaPlayer(gid, 1, TEST_GUILD_ID, MafiaRole.MAFIA),
            MafiaPlayer(gid, 2, TEST_GUILD_ID, MafiaRole.TOWNIE),
            MafiaPlayer(gid, 3, TEST_GUILD_ID, MafiaRole.TOWNIE),
        ],
    )
    mafia_repo.record_action(gid, TEST_GUILD_ID, 1, 2, MafiaActionType.KILL, MafiaPhase.NIGHT)
    mafia_repo.record_action(gid, TEST_GUILD_ID, 1, 3, MafiaActionType.KILL, MafiaPhase.NIGHT)

    actions = mafia_repo.get_actions(gid, MafiaActionType.KILL, MafiaPhase.NIGHT)
    assert len(actions) == 1
    assert actions[0]["target_id"] == 3

    actor = mafia_repo.get_player(gid, 1)
    assert actor.acted is True


def test_get_actions_filters_by_type_and_phase(mafia_repo):
    gid = mafia_repo.create_game(TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 0, 3, None)
    mafia_repo.add_players(
        gid,
        [
            MafiaPlayer(gid, 1, TEST_GUILD_ID, MafiaRole.MAFIA),
            MafiaPlayer(gid, 2, TEST_GUILD_ID, MafiaRole.DETECTIVE),
        ],
    )
    mafia_repo.record_action(gid, TEST_GUILD_ID, 1, 2, MafiaActionType.KILL, MafiaPhase.NIGHT)
    mafia_repo.record_action(
        gid, TEST_GUILD_ID, 2, 1, MafiaActionType.INVESTIGATE, MafiaPhase.NIGHT, result="Mafia"
    )

    kills = mafia_repo.get_actions(gid, MafiaActionType.KILL)
    assert len(kills) == 1

    invests = mafia_repo.get_actions(gid, MafiaActionType.INVESTIGATE, MafiaPhase.NIGHT)
    assert invests[0]["result"] == "Mafia"


# ── Eligibility ───────────────────────────────────────────────────────────


def test_eligibility_union_gamba_and_dig(mafia_repo):
    now = int(time.time())
    _seed_wheel_spin(mafia_repo, discord_id=100, guild_id=TEST_GUILD_ID, ts=now - 1000)
    _seed_dig_action(mafia_repo, actor_id=200, guild_id=TEST_GUILD_ID, ts=now - 500)
    # Out-of-window - should be excluded
    _seed_wheel_spin(mafia_repo, discord_id=300, guild_id=TEST_GUILD_ID, ts=now - 86400 * 5)
    # Other guild - should be excluded
    _seed_dig_action(mafia_repo, actor_id=400, guild_id=TEST_GUILD_ID_SECONDARY, ts=now - 100)

    ids = mafia_repo.get_eligible_player_ids(TEST_GUILD_ID, since=now - 86400)
    assert set(ids) == {100, 200}


def test_eligibility_excludes_optout(mafia_repo):
    now = int(time.time())
    _seed_wheel_spin(mafia_repo, discord_id=100, guild_id=TEST_GUILD_ID, ts=now - 1000)
    _seed_dig_action(mafia_repo, actor_id=200, guild_id=TEST_GUILD_ID, ts=now - 1000)
    mafia_repo.set_optout(TEST_GUILD_ID, 200, True)

    ids = mafia_repo.get_eligible_player_ids(TEST_GUILD_ID, since=now - 86400)
    assert ids == [100]


def test_eligibility_dedup_across_sources(mafia_repo):
    now = int(time.time())
    _seed_wheel_spin(mafia_repo, discord_id=100, guild_id=TEST_GUILD_ID, ts=now - 100)
    _seed_dig_action(mafia_repo, actor_id=100, guild_id=TEST_GUILD_ID, ts=now - 200)

    ids = mafia_repo.get_eligible_player_ids(TEST_GUILD_ID, since=now - 86400)
    assert ids == [100]


def test_eligibility_excludes_unregistered_activity(mafia_repo):
    now = int(time.time())
    _seed_dig_action(
        mafia_repo,
        actor_id=100,
        guild_id=TEST_GUILD_ID,
        ts=now - 100,
        registered=False,
    )

    ids = mafia_repo.get_eligible_player_ids(TEST_GUILD_ID, since=now - 86400)
    assert ids == []


def test_eligibility_excludes_players_past_entry_fee_debt_floor(mafia_repo):
    now = int(time.time())
    _seed_dig_action(mafia_repo, actor_id=100, guild_id=TEST_GUILD_ID, ts=now - 100)
    _seed_registered_player(
        mafia_repo, discord_id=100, guild_id=TEST_GUILD_ID, balance=-480
    )

    ids = mafia_repo.get_eligible_player_ids(
        TEST_GUILD_ID,
        since=now - 86400,
        entry_fee=30,
        max_debt=500,
    )
    assert ids == []


# ── Optout ────────────────────────────────────────────────────────────────


def test_optout_toggle(mafia_repo):
    assert mafia_repo.is_opted_out(TEST_GUILD_ID, 100) is False
    mafia_repo.set_optout(TEST_GUILD_ID, 100, True)
    assert mafia_repo.is_opted_out(TEST_GUILD_ID, 100) is True
    mafia_repo.set_optout(TEST_GUILD_ID, 100, False)
    assert mafia_repo.is_opted_out(TEST_GUILD_ID, 100) is False


# ── Participation history ─────────────────────────────────────────────────


def test_recent_player_participation(mafia_repo):
    for i, date in enumerate(["2026-04-21", "2026-04-22", "2026-04-23", "2026-04-24"]):
        gid = mafia_repo.create_game(TEST_GUILD_ID, date, MafiaPhase.RESOLVED, i * 100, 5, None)
        mafia_repo.add_players(
            gid,
            [
                MafiaPlayer(
                    gid, 100, TEST_GUILD_ID, MafiaRole.TOWNIE,
                    acted=(i in (0, 3)),  # only first and last
                ),
            ],
        )
        mafia_repo.finalize_game(gid, MafiaWinner.TOWN, 40, mvp_id=None)

    recent = mafia_repo.get_recent_player_participation(100, TEST_GUILD_ID, limit=3)
    # Newest first → game_date 2026-04-24, 23, 22 → acted: True, False, False
    assert recent == [True, False, False]


# ── Finalize / leaderboard / stats ────────────────────────────────────────


def test_finalize_and_leaderboard(mafia_repo):
    # Game 1: town wins, player 100 was townie
    gid1 = mafia_repo.create_game(TEST_GUILD_ID, "2026-04-22", MafiaPhase.NIGHT, 0, 5, None)
    mafia_repo.add_players(
        gid1,
        [
            MafiaPlayer(gid1, 100, TEST_GUILD_ID, MafiaRole.TOWNIE),
            MafiaPlayer(gid1, 200, TEST_GUILD_ID, MafiaRole.MAFIA),
        ],
    )
    mafia_repo.finalize_game(gid1, MafiaWinner.TOWN, 40, mvp_id=100)

    # Game 2: mafia wins, player 100 was townie (loss)
    gid2 = mafia_repo.create_game(TEST_GUILD_ID, "2026-04-23", MafiaPhase.NIGHT, 100, 5, None)
    mafia_repo.add_players(
        gid2,
        [
            MafiaPlayer(gid2, 100, TEST_GUILD_ID, MafiaRole.TOWNIE),
            MafiaPlayer(gid2, 200, TEST_GUILD_ID, MafiaRole.MAFIA),
        ],
    )
    mafia_repo.finalize_game(gid2, MafiaWinner.MAFIA, 40, mvp_id=200)

    rows = mafia_repo.get_leaderboard(TEST_GUILD_ID, limit=10)
    by_id = {r["discord_id"]: r for r in rows}
    assert by_id[100]["games_played"] == 2
    assert by_id[100]["wins"] == 1
    assert by_id[100]["mvp_count"] == 1
    assert by_id[200]["mafia_wins"] == 1


def test_compute_player_stats_correct_reads(mafia_repo):
    gid = mafia_repo.create_game(TEST_GUILD_ID, "2026-04-24", MafiaPhase.NIGHT, 0, 5, None)
    detective_id, mafia_id = 100, 200
    mafia_repo.add_players(
        gid,
        [
            MafiaPlayer(gid, detective_id, TEST_GUILD_ID, MafiaRole.DETECTIVE),
            MafiaPlayer(
                gid, mafia_id, TEST_GUILD_ID, MafiaRole.MAFIA, is_godfather=False
            ),
            MafiaPlayer(gid, 300, TEST_GUILD_ID, MafiaRole.TOWNIE),
        ],
    )
    mafia_repo.record_action(
        gid, TEST_GUILD_ID, detective_id, mafia_id,
        MafiaActionType.INVESTIGATE, MafiaPhase.NIGHT, result="Mafia",
    )
    mafia_repo.set_player_alive(
        gid, mafia_id, False, eliminated_phase=MafiaPhase.DAY
    )
    mafia_repo.finalize_game(gid, MafiaWinner.TOWN, 40, mvp_id=detective_id)

    stats = mafia_repo.compute_player_stats(TEST_GUILD_ID, detective_id)
    assert stats["correct_reads"] == 1
    assert stats["wins"] == 1
    assert stats["town_wins"] == 1
