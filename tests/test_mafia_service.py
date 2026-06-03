"""Tests for MafiaService."""

from __future__ import annotations

import random
import time
from unittest.mock import MagicMock

import pytest

from domain.models.mafia import (
    MafiaActionType,
    MafiaPhase,
    MafiaRole,
    MafiaTwist,
    MafiaWinner,
)
from repositories.mafia_repository import MafiaRepository
from repositories.player_repository import PlayerRepository
from services.mafia_service import (
    MIN_ROSTER,
    MVP_BONUS,
    PAYOUT_BASE,
    PAYOUT_PER_EXTRA,
    ROLE_TABLE,
    MafiaService,
    _payout_for_roster,
)
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


class FakeHeroProvider:
    def __init__(self, names: list[str] | None = None):
        self._names = names or [f"Hero{i}" for i in range(50)]

    def all_hero_names(self):
        return self._names

    def sample_unique(self, n: int) -> list[str]:
        if n <= len(self._names):
            return list(self._names[:n])
        return [self._names[i % len(self._names)] for i in range(n)]


class FakeDigService:
    def __init__(self, date: str = "2026-04-24"):
        self._date = date

    def _get_game_date(self) -> str:
        return self._date


@pytest.fixture
def mafia_repo(repo_db_path):
    return MafiaRepository(repo_db_path)


@pytest.fixture
def player_repo(repo_db_path):
    return PlayerRepository(repo_db_path)


@pytest.fixture
def flavor_service():
    svc = MagicMock()
    return svc


@pytest.fixture
def mafia_service(mafia_repo, player_repo, flavor_service):
    return MafiaService(
        mafia_repo=mafia_repo,
        player_repo=player_repo,
        dig_service=FakeDigService(),
        flavor_service=flavor_service,
        hero_provider=FakeHeroProvider(),
        rng=random.Random(42),
    )


def _seed_player(player_repo, discord_id: int, balance: int = 0) -> None:
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"user_{discord_id}",
        guild_id=TEST_GUILD_ID,
    )
    if balance:
        player_repo.add_balance(discord_id, TEST_GUILD_ID, balance)


def _seed_eligible_via_dig(mafia_repo, ids: list[int], guild_id: int = TEST_GUILD_ID) -> None:
    now = int(time.time())
    with mafia_repo.connection() as conn:
        cursor = conn.cursor()
        for pid in ids:
            cursor.execute(
                """
                INSERT INTO dig_actions (guild_id, actor_id, target_id, action_type,
                    depth_before, depth_after, jc_delta, detail, created_at)
                VALUES (?, ?, NULL, 'dig', 0, 0, 0, NULL, ?)
                """,
                (guild_id, pid, now - 100),
            )


# ── Lifecycle ─────────────────────────────────────────────────────────────


def test_below_min_returns_none(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, [1, 2, 3])  # only 3 < MIN_ROSTER=5
    result = mafia_service.start_daily_game(TEST_GUILD_ID)
    assert result is None
    assert mafia_repo.get_game_for_date(TEST_GUILD_ID, "2026-04-24") is None


def test_idempotent_start(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, list(range(101, 110)))  # 9 players
    first = mafia_service.start_daily_game(TEST_GUILD_ID)
    assert first is not None
    second = mafia_service.start_daily_game(TEST_GUILD_ID)
    assert second is not None
    assert first.game_id == second.game_id


@pytest.mark.parametrize("roster_size", list(range(5, 16)))
def test_role_assignment_table(mafia_repo, mafia_service, roster_size):
    ids = list(range(101, 101 + roster_size))
    _seed_eligible_via_dig(mafia_repo, ids)
    # Force jester probability to 0 for this test (deterministic counts)
    mafia_service._rng = random.Random(0)
    # Patch JESTER_PROBABILITY indirectly: we assert ignoring jester swap below
    game = mafia_service.start_daily_game(TEST_GUILD_ID)
    assert game is not None

    players = mafia_repo.get_players(game.game_id)
    counts: dict[MafiaRole, int] = dict.fromkeys(MafiaRole, 0)
    for p in players:
        counts[p.role] += 1

    expected = ROLE_TABLE[roster_size]
    # Jester (if rolled) replaces a townie. So mafia/doctor/detective/vigilante are exact;
    # townie + jester sums to remainder.
    assert counts[MafiaRole.MAFIA] == expected["mafia"]
    assert counts[MafiaRole.DOCTOR] == expected["doctor"]
    assert counts[MafiaRole.DETECTIVE] == expected["detective"]
    assert counts[MafiaRole.VIGILANTE] == expected["vigilante"]
    townie_pool = (
        roster_size
        - expected["mafia"]
        - expected["doctor"]
        - expected["detective"]
        - expected["vigilante"]
    )
    assert counts[MafiaRole.TOWNIE] + counts[MafiaRole.JESTER] == townie_pool


def test_godfather_always_among_mafia(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, list(range(101, 113)))  # 12 players → 3 mafia
    game = mafia_service.start_daily_game(TEST_GUILD_ID)
    players = mafia_repo.get_players(game.game_id)
    gfs = [p for p in players if p.is_godfather]
    assert len(gfs) == 1
    assert gfs[0].role == MafiaRole.MAFIA


def test_oversized_roster_capped(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, list(range(101, 131)))  # 30 players
    game = mafia_service.start_daily_game(TEST_GUILD_ID)
    assert game.roster_size == 15


def test_optout_excludes_player(mafia_repo, mafia_service):
    ids = list(range(101, 110))
    _seed_eligible_via_dig(mafia_repo, ids)
    mafia_service.set_optout(TEST_GUILD_ID, 105, True)
    game = mafia_service.start_daily_game(TEST_GUILD_ID)
    rostered = {p.discord_id for p in mafia_repo.get_players(game.game_id)}
    assert 105 not in rostered


def test_guild_isolation(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, list(range(101, 110)))  # guild A
    game_a = mafia_service.start_daily_game(TEST_GUILD_ID)
    assert game_a is not None
    # Guild B has no eligible players → no game
    game_b = mafia_service.start_daily_game(TEST_GUILD_ID_SECONDARY)
    assert game_b is None


# ── Action submission ─────────────────────────────────────────────────────


def _new_game(mafia_repo, players: list[tuple[int, MafiaRole, bool]], date: str = "2026-04-24"):
    """Helper to create a game and roster directly without service."""
    from domain.models.mafia import MafiaPlayer

    gid = mafia_repo.create_game(TEST_GUILD_ID, date, MafiaPhase.NIGHT, int(time.time()), len(players), None)
    mp = []
    for pid, role, gf in players:
        mp.append(
            MafiaPlayer(
                game_id=gid,
                discord_id=pid,
                guild_id=TEST_GUILD_ID,
                role=role,
                is_godfather=gf,
                hero_name=f"Hero{pid}",
            )
        )
    mafia_repo.add_players(gid, mp)
    return gid


def test_night_action_role_validation(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.TOWNIE, False),
        ],
    )
    # Townie can't kill
    res = mafia_service.submit_night_action(TEST_GUILD_ID, 2, 1, MafiaActionType.KILL)
    assert res["ok"] is False


def test_mafia_cannot_kill_mafia(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.MAFIA, False),
            (3, MafiaRole.TOWNIE, False),
        ],
    )
    res = mafia_service.submit_night_action(TEST_GUILD_ID, 1, 2, MafiaActionType.KILL)
    assert res["ok"] is False


def test_detective_godfather_reads_as_town(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.DETECTIVE, False),
            (2, MafiaRole.MAFIA, True),  # Godfather
            (3, MafiaRole.MAFIA, False),
            (4, MafiaRole.TOWNIE, False),
        ],
    )
    on_gf = mafia_service.submit_night_action(TEST_GUILD_ID, 1, 2, MafiaActionType.INVESTIGATE)
    assert on_gf["result"] == "Town"

    on_regular = mafia_service.submit_night_action(TEST_GUILD_ID, 1, 3, MafiaActionType.INVESTIGATE)
    assert on_regular["result"] == "Mafia"


def test_vigilante_one_shot(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.VIGILANTE, False),
            (2, MafiaRole.TOWNIE, False),
            (3, MafiaRole.TOWNIE, False),
        ],
    )
    res1 = mafia_service.submit_night_action(TEST_GUILD_ID, 1, 2, MafiaActionType.VIG_KILL)
    assert res1["ok"]
    res2 = mafia_service.submit_night_action(TEST_GUILD_ID, 1, 3, MafiaActionType.VIG_KILL)
    assert res2["ok"] is False


# ── Night resolution ─────────────────────────────────────────────────────


def test_doctor_save_blocks_kill(mafia_service, mafia_repo):
    gid = _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.DOCTOR, False),
            (3, MafiaRole.TOWNIE, False),
            (4, MafiaRole.TOWNIE, False),
            (5, MafiaRole.DETECTIVE, False),
        ],
    )
    mafia_service.submit_night_action(TEST_GUILD_ID, 1, 3, MafiaActionType.KILL)
    mafia_service.submit_night_action(TEST_GUILD_ID, 2, 3, MafiaActionType.SAVE)

    summary = mafia_service.resolve_night(TEST_GUILD_ID)
    assert summary["resolved"]
    assert summary["killed"] == []  # save blocked the kill

    target = mafia_repo.get_player(gid, 3)
    assert target.is_alive is True


def test_blood_moon_kills_two(mafia_service, mafia_repo):
    gid = _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.MAFIA, False),
            (3, MafiaRole.DOCTOR, False),
            (4, MafiaRole.TOWNIE, False),
            (5, MafiaRole.TOWNIE, False),
            (6, MafiaRole.TOWNIE, False),
            (7, MafiaRole.DETECTIVE, False),
        ],
    )
    # Set Blood Moon manually
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET twist_event = ? WHERE game_id = ?",
            (MafiaTwist.BLOOD_MOON.value, gid),
        )

    # Two mafia each pick a different target
    mafia_service.submit_night_action(TEST_GUILD_ID, 1, 4, MafiaActionType.KILL)
    mafia_service.submit_night_action(TEST_GUILD_ID, 2, 5, MafiaActionType.KILL)

    summary = mafia_service.resolve_night(TEST_GUILD_ID)
    assert summary["resolved"]
    assert len(summary["killed"]) == 2


def test_plague_kills_extra_unblockable(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.DOCTOR, False),
            (3, MafiaRole.TOWNIE, False),
            (4, MafiaRole.TOWNIE, False),
            (5, MafiaRole.TOWNIE, False),
            (6, MafiaRole.DETECTIVE, False),
        ],
    )
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET twist_event = 'PLAGUE' WHERE game_id = (SELECT MAX(game_id) FROM mafia_games)",
        )

    mafia_service.submit_night_action(TEST_GUILD_ID, 1, 3, MafiaActionType.KILL)

    summary = mafia_service.resolve_night(TEST_GUILD_ID)
    assert summary["resolved"]
    # Mafia kill + plague kill = 2 deaths
    assert len(summary["killed"]) == 2


def test_no_kill_submission_picks_random_alive_non_mafia(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.TOWNIE, False),
            (3, MafiaRole.DOCTOR, False),
            (4, MafiaRole.DETECTIVE, False),
            (5, MafiaRole.TOWNIE, False),
        ],
    )
    summary = mafia_service.resolve_night(TEST_GUILD_ID)
    assert summary["resolved"]
    assert len(summary["killed"]) == 1
    victim_id = summary["killed"][0]["discord_id"]
    assert victim_id in {2, 3, 4, 5}


# ── Day resolution / win conditions ──────────────────────────────────────


def _force_phase(mafia_repo, phase: MafiaPhase) -> None:
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET phase = ? WHERE game_id = (SELECT MAX(game_id) FROM mafia_games)",
            (phase.value,),
        )


def test_lynch_plurality(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.TOWNIE, False),
            (3, MafiaRole.DOCTOR, False),
            (4, MafiaRole.DETECTIVE, False),
            (5, MafiaRole.TOWNIE, False),
        ],
    )
    _force_phase(mafia_repo, MafiaPhase.DAY)

    # Three votes for player 1 (the mafia), one for player 2.
    mafia_service.submit_day_vote(TEST_GUILD_ID, 2, 1)
    mafia_service.submit_day_vote(TEST_GUILD_ID, 3, 1)
    mafia_service.submit_day_vote(TEST_GUILD_ID, 4, 1)
    mafia_service.submit_day_vote(TEST_GUILD_ID, 5, 2)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["resolved"]
    assert summary["lynched_id"] == 1
    assert summary["winner"] == MafiaWinner.TOWN.value


def test_lynch_tie_no_lynch(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.TOWNIE, False),
            (3, MafiaRole.TOWNIE, False),
            (4, MafiaRole.DOCTOR, False),
            (5, MafiaRole.DETECTIVE, False),
        ],
    )
    _force_phase(mafia_repo, MafiaPhase.DAY)

    mafia_service.submit_day_vote(TEST_GUILD_ID, 2, 1)
    mafia_service.submit_day_vote(TEST_GUILD_ID, 3, 4)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["lynched_id"] is None


def test_jester_win(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.JESTER, False),
            (3, MafiaRole.TOWNIE, False),
            (4, MafiaRole.DOCTOR, False),
            (5, MafiaRole.DETECTIVE, False),
        ],
    )
    _force_phase(mafia_repo, MafiaPhase.DAY)
    # Lynch the jester
    for voter in (1, 3, 4, 5):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 2)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["winner"] == MafiaWinner.JESTER.value
    assert summary["mvp_id"] == 2


def test_town_hall_blocks_lynch(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.TOWNIE, False),
            (3, MafiaRole.DOCTOR, False),
            (4, MafiaRole.DETECTIVE, False),
            (5, MafiaRole.TOWNIE, False),
        ],
    )
    _force_phase(mafia_repo, MafiaPhase.DAY)
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET twist_event = 'TOWN_HALL' WHERE game_id = (SELECT MAX(game_id) FROM mafia_games)",
        )

    for voter in (2, 3, 4, 5):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 1)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["lynched_id"] is None
    # Mafia is alive; town isn't outnumbered (4 vs 1) so… mafia wins anyway by attrition rule
    # Actually with 1 mafia, 4 non-mafia: mafia not >= non-mafia, so winner is MAFIA only if rule
    # Let's just assert it resolved without lynch.
    assert summary["resolved"]


# ── Payouts ───────────────────────────────────────────────────────────────


def test_payout_amount_scales(mafia_service, mafia_repo, player_repo):
    # Roster of 7 → payout = 40 + 8*2 = 56
    ids = [101, 102, 103, 104, 105, 106, 107]
    for pid in ids:
        _seed_player(player_repo, pid, balance=0)
    # Snapshot starting balances (default jopacoin_balance is non-zero)
    starting = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids}

    # Build a game with majority town and one mafia
    gid = _new_game(
        mafia_repo,
        [
            (101, MafiaRole.MAFIA, True),
            (102, MafiaRole.TOWNIE, False),
            (103, MafiaRole.TOWNIE, False),
            (104, MafiaRole.TOWNIE, False),
            (105, MafiaRole.DOCTOR, False),
            (106, MafiaRole.DETECTIVE, False),
            (107, MafiaRole.TOWNIE, False),
        ],
    )
    # Force roster_size to 7 so payout calculation is correct
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET roster_size = 7 WHERE game_id = ?", (gid,)
        )
    _force_phase(mafia_repo, MafiaPhase.DAY)

    # All non-mafia vote for the mafia
    for voter in (102, 103, 104, 105, 106, 107):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 101)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["winner"] == MafiaWinner.TOWN.value
    expected = PAYOUT_BASE + PAYOUT_PER_EXTRA * (7 - MIN_ROSTER)
    assert summary["payout_per_winner"] == expected

    # Each non-MVP winner has +expected; MVP has +expected+MVP_BONUS
    mvp_id = summary["mvp_id"]
    for pid in [102, 103, 104, 105, 106, 107]:
        delta = player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid]
        if pid == mvp_id:
            assert delta == expected + MVP_BONUS
        else:
            assert delta == expected
    # Mafia got nothing
    assert player_repo.get_balance(101, TEST_GUILD_ID) == starting[101]


def test_payout_for_roster_helper():
    assert _payout_for_roster(5) == 40
    assert _payout_for_roster(10) == 80
    assert _payout_for_roster(15) == 120


# ── Auto-skip ─────────────────────────────────────────────────────────────


def test_auto_skip_after_three_misses(mafia_repo, mafia_service):
    from domain.models.mafia import MafiaPlayer

    pid = 999
    for date in ("2026-04-21", "2026-04-22", "2026-04-23"):
        gid = mafia_repo.create_game(
            TEST_GUILD_ID, date, MafiaPhase.RESOLVED, 0, 5, None
        )
        mafia_repo.add_players(
            gid,
            [MafiaPlayer(gid, pid, TEST_GUILD_ID, MafiaRole.TOWNIE, acted=False)],
        )
        mafia_repo.finalize_game(gid, MafiaWinner.TOWN, 40, mvp_id=None)

    assert mafia_service.is_active_for_auto_roster(TEST_GUILD_ID, pid) is False


def test_auto_skip_resets_on_action(mafia_repo, mafia_service):
    from domain.models.mafia import MafiaPlayer

    pid = 999
    # Two misses then one action
    for i, date in enumerate(("2026-04-21", "2026-04-22", "2026-04-23")):
        gid = mafia_repo.create_game(
            TEST_GUILD_ID, date, MafiaPhase.RESOLVED, 0, 5, None
        )
        mafia_repo.add_players(
            gid,
            [MafiaPlayer(gid, pid, TEST_GUILD_ID, MafiaRole.TOWNIE, acted=(i == 2))],
        )
        mafia_repo.finalize_game(gid, MafiaWinner.TOWN, 40, mvp_id=None)

    assert mafia_service.is_active_for_auto_roster(TEST_GUILD_ID, pid) is True


# ── Status & role views ───────────────────────────────────────────────────


def test_status_hides_breakdown_during_day(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.TOWNIE, False),
            (3, MafiaRole.DOCTOR, False),
            (4, MafiaRole.DETECTIVE, False),
            (5, MafiaRole.TOWNIE, False),
        ],
    )
    _force_phase(mafia_repo, MafiaPhase.DAY)
    mafia_service.submit_day_vote(TEST_GUILD_ID, 2, 1)
    mafia_service.submit_day_vote(TEST_GUILD_ID, 3, 1)

    s = mafia_service.get_public_status(TEST_GUILD_ID)
    assert s["active"]
    assert s["voted_count"] == 2
    # No vote_breakdown key in public status
    assert "vote_breakdown" not in s


def test_player_role_includes_allies_for_mafia(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.MAFIA, False),
            (3, MafiaRole.TOWNIE, False),
            (4, MafiaRole.DOCTOR, False),
            (5, MafiaRole.DETECTIVE, False),
        ],
    )
    info = mafia_service.get_player_role(TEST_GUILD_ID, 1)
    assert info["role"] == MafiaRole.MAFIA.value
    assert info["is_godfather"] is True
    ally_ids = {a["discord_id"] for a in info["allies"]}
    assert ally_ids == {2}


def test_player_role_for_non_player_returns_none(mafia_service, mafia_repo):
    _new_game(mafia_repo, [(1, MafiaRole.MAFIA, True), (2, MafiaRole.TOWNIE, False),
                            (3, MafiaRole.TOWNIE, False), (4, MafiaRole.DOCTOR, False),
                            (5, MafiaRole.DETECTIVE, False)])
    info = mafia_service.get_player_role(TEST_GUILD_ID, 9999)
    assert info is None
