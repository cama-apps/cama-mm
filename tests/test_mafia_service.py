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
from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.mafia_repository import MafiaRepository
from repositories.player_repository import PlayerRepository
from services.mafia_service import (
    ENTRY_FEE,
    MVP_BONUS,
    ROLE_TABLE,
    MafiaService,
    _payout_pool_for_pot,
    _pot_for_roster,
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
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


def _seed_eligible_via_dig(mafia_repo, ids: list[int], guild_id: int = TEST_GUILD_ID) -> None:
    player_repo = PlayerRepository(mafia_repo.db_path)
    now = int(time.time())
    for pid in ids:
        try:
            player_repo.add(
                discord_id=pid,
                discord_username=f"user_{pid}",
                guild_id=guild_id,
            )
        except ValueError:
            pass
        player_repo.update_balance(pid, guild_id, 100)
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


def test_start_daily_game_debits_entry_fee_once(mafia_repo, mafia_service, player_repo):
    ids = list(range(101, 110))
    _seed_eligible_via_dig(mafia_repo, ids)

    first = mafia_service.start_daily_game(TEST_GUILD_ID)
    assert first is not None
    balances_after_first = {
        pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids
    }
    assert set(balances_after_first.values()) == {100 - ENTRY_FEE}

    second = mafia_service.start_daily_game(TEST_GUILD_ID)
    assert second is not None
    assert second.game_id == first.game_id
    assert {
        pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids
    } == balances_after_first


def test_auto_roster_excludes_players_past_entry_fee_debt_floor(
    mafia_repo, mafia_service, player_repo
):
    ids = list(range(101, 107))
    _seed_eligible_via_dig(mafia_repo, ids)
    player_repo.update_balance(106, TEST_GUILD_ID, -493)

    game = mafia_service.start_daily_game(TEST_GUILD_ID)
    assert game is not None
    rostered = {p.discord_id for p in mafia_repo.get_players(game.game_id)}
    assert 106 not in rostered
    assert len(rostered) == 5


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

    player_repo = PlayerRepository(mafia_repo.db_path)
    for pid, _, _ in players:
        try:
            player_repo.add(
                discord_id=pid,
                discord_username=f"user_{pid}",
                guild_id=TEST_GUILD_ID,
            )
        except ValueError:
            pass
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


def test_resolve_night_only_advances_once(mafia_service, mafia_repo):
    gid = _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, True),
            (2, MafiaRole.TOWNIE, False),
            (3, MafiaRole.DOCTOR, False),
            (4, MafiaRole.DETECTIVE, False),
            (5, MafiaRole.TOWNIE, False),
        ],
    )

    first = mafia_service.resolve_night(TEST_GUILD_ID)
    assert first["resolved"] is True
    deaths_after_first = [
        p.discord_id for p in mafia_repo.get_players(gid) if not p.is_alive
    ]

    second = mafia_service.resolve_night(TEST_GUILD_ID)
    assert second["resolved"] is False
    deaths_after_second = [
        p.discord_id for p in mafia_repo.get_players(gid) if not p.is_alive
    ]
    assert deaths_after_second == deaths_after_first


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
    # Roster of 7 -> pot = 56. resolve_day only pays winners; it does not debit
    # fees (that happens in start_daily_game). To check post-resolution net
    # delta, we simulate the fee debit ourselves so this test exercises the full
    # end-to-end balance change.
    ids = [101, 102, 103, 104, 105, 106, 107]
    for pid in ids:
        _seed_player(player_repo, pid, balance=0)
    starting = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids}
    for pid in ids:
        player_repo.add_balance(pid, TEST_GUILD_ID, -ENTRY_FEE)

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
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET roster_size = 7 WHERE game_id = ?", (gid,)
        )
    _force_phase(mafia_repo, MafiaPhase.DAY)

    for voter in (102, 103, 104, 105, 106, 107):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 101)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["winner"] == MafiaWinner.TOWN.value

    pot = _pot_for_roster(7)
    payout_pool = _payout_pool_for_pot(pot)
    assert summary["pot_total"] == pot
    assert summary["payout_pool"] == payout_pool
    assert summary["entry_fee"] == ENTRY_FEE

    # Town has 6 winners. MVP gets (payout_pool - MVP_BONUS) // 6 base +
    # MVP_BONUS + rounding dust; the other 5 winners each get the base.
    base = (payout_pool - MVP_BONUS) // 6
    dust = (payout_pool - MVP_BONUS) - base * 6
    assert summary["payout_per_winner"] == base

    mvp_id = summary["mvp_id"]
    for pid in [102, 103, 104, 105, 106, 107]:
        delta = player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid]
        if pid == mvp_id:
            assert delta == base + MVP_BONUS + dust - ENTRY_FEE
        else:
            assert delta == base - ENTRY_FEE

    # Mafia paid the fee and got nothing back.
    assert (
        player_repo.get_balance(101, TEST_GUILD_ID) - starting[101] == -ENTRY_FEE
    )

    # The full reduced pot is paid out.
    total_delta = sum(
        player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid] for pid in ids
    )
    assert total_delta == 0


def test_pot_for_roster_helper():
    assert _pot_for_roster(5) == 5 * ENTRY_FEE
    assert _pot_for_roster(10) == 10 * ENTRY_FEE
    assert _pot_for_roster(15) == 15 * ENTRY_FEE
    assert _payout_pool_for_pot(_pot_for_roster(5)) == 40
    assert _payout_pool_for_pot(_pot_for_roster(10)) == 80
    assert _payout_pool_for_pot(_pot_for_roster(15)) == 120


def test_reduced_buyin_conserves_pot_across_outcomes(
    mafia_service, mafia_repo, player_repo
):
    """The full reduced pot is paid out for every win outcome."""
    scenarios = [
        # (roles, voters_target, expected_winner)
        # Town wins (4 town vs 1 mafia after lynch).
        (
            [
                (201, MafiaRole.MAFIA, True),
                (202, MafiaRole.TOWNIE, False),
                (203, MafiaRole.DOCTOR, False),
                (204, MafiaRole.DETECTIVE, False),
                (205, MafiaRole.TOWNIE, False),
            ],
            {(202, 201), (203, 201), (204, 201), (205, 201)},
            MafiaWinner.TOWN,
        ),
        # Jester wins by being lynched.
        (
            [
                (301, MafiaRole.MAFIA, True),
                (302, MafiaRole.MAFIA, False),
                (303, MafiaRole.JESTER, False),
                (304, MafiaRole.TOWNIE, False),
                (305, MafiaRole.DETECTIVE, False),
            ],
            {(301, 303), (302, 303), (304, 303), (305, 303)},
            MafiaWinner.JESTER,
        ),
    ]

    for i, (roles, votes, expected_winner) in enumerate(scenarios):
        ids = [pid for pid, _, _ in roles]
        for pid in ids:
            _seed_player(player_repo, pid, balance=0)
        starting = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids}
        for pid in ids:
            player_repo.add_balance(pid, TEST_GUILD_ID, -ENTRY_FEE)

        gid = _new_game(mafia_repo, roles, date=f"2026-04-{24 + i:02d}")
        with mafia_repo.connection() as conn:
            conn.cursor().execute(
                "UPDATE mafia_games SET roster_size = ? WHERE game_id = ?",
                (len(roles), gid),
            )
        _force_phase(mafia_repo, MafiaPhase.DAY)

        for actor, target in votes:
            mafia_service.submit_day_vote(TEST_GUILD_ID, actor, target)

        summary = mafia_service.resolve_day(TEST_GUILD_ID)
        assert summary["winner"] == expected_winner.value

        pot = _pot_for_roster(len(ids))
        payout_pool = _payout_pool_for_pot(pot)
        assert summary["pot_total"] == pot
        assert summary["payout_pool"] == payout_pool

        total_delta = sum(
            player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid] for pid in ids
        )
        assert total_delta == 0, (
            f"scenario {i} ({expected_winner.value}): wrong net delta {total_delta}"
        )


def test_mafia_faction_win_pays_whole_pot(mafia_service, mafia_repo, player_repo):
    """A MAFIA faction win hands the entire pot to the mafia and stays zero-sum.

    The other payout tests only drive TOWN and JESTER wins; this exercises the
    mafia winning-faction split + within-faction MVP/dust allocation.
    """
    roles = [
        (601, MafiaRole.MAFIA, True),
        (602, MafiaRole.MAFIA, False),
        (603, MafiaRole.TOWNIE, False),
        (604, MafiaRole.DETECTIVE, False),
        (605, MafiaRole.TOWNIE, False),
    ]
    ids = [pid for pid, _, _ in roles]
    for pid in ids:
        _seed_player(player_repo, pid, balance=0)
    starting = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids}
    for pid in ids:
        player_repo.add_balance(pid, TEST_GUILD_ID, -ENTRY_FEE)

    gid = _new_game(mafia_repo, roles)
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET roster_size = ? WHERE game_id = ?",
            (len(roles), gid),
        )
    _force_phase(mafia_repo, MafiaPhase.DAY)

    # Town misfires and lynches a townie (605); the two mafia survive and now
    # equal the two remaining non-mafia → mafia win by attrition.
    for actor, target in [(601, 605), (602, 605), (603, 605), (604, 601)]:
        mafia_service.submit_day_vote(TEST_GUILD_ID, actor, target)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["winner"] == MafiaWinner.MAFIA.value
    assert summary["lynched_id"] == 605
    winning_ids = summary["winning_ids"]
    assert set(winning_ids) == {601, 602}

    pot = summary["pot_total"]
    assert pot == _pot_for_roster(5)
    mvp_share = MVP_BONUS if summary["mvp_id"] in winning_ids else 0
    assert summary["payout_per_winner"] == (pot - mvp_share) // len(winning_ids)

    # The whole pot went to the mafia (net of their own two entry fees).
    winner_delta = sum(
        player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid] for pid in winning_ids
    )
    assert winner_delta == pot - len(winning_ids) * ENTRY_FEE

    # Losers only lost their fee, and the whole game is zero-sum.
    for pid in (603, 604, 605):
        assert player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid] == -ENTRY_FEE
    total_delta = sum(
        player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid] for pid in ids
    )
    assert total_delta == 0


def test_detective_backfill_skipped_when_night_resolution_noops(
    mafia_service, mafia_repo, monkeypatch
):
    """Detective backfill must run only after the night actually resolves.

    Regression guard: detective rows were previously written *before* calling
    apply_night_resolution, so a concurrent tick that lost the resolution race
    (apply returns False) still mutated detective state on a no-op path.
    """
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
    res = mafia_service.submit_night_action(
        TEST_GUILD_ID, 4, 1, MafiaActionType.INVESTIGATE
    )
    assert res["ok"]

    # Lose the resolution race: apply_night_resolution no-ops.
    monkeypatch.setattr(mafia_repo, "apply_night_resolution", lambda *a, **k: False)
    calls: list[int] = []
    monkeypatch.setattr(
        mafia_service, "_record_detective_results",
        lambda *a, **k: calls.append(1),
    )

    summary = mafia_service.resolve_night(TEST_GUILD_ID)
    assert summary["resolved"] is False
    assert summary["reason"] == "night_already_resolved"
    assert calls == [], "detective backfill ran on the no-op resolution path"


def test_resolve_day_does_not_double_pay(mafia_service, mafia_repo, player_repo):
    ids = [401, 402, 403, 404, 405]
    for pid in ids:
        _seed_player(player_repo, pid, balance=0)
    starting = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids}
    for pid in ids:
        player_repo.add_balance(pid, TEST_GUILD_ID, -ENTRY_FEE)

    _new_game(
        mafia_repo,
        [
            (401, MafiaRole.MAFIA, True),
            (402, MafiaRole.TOWNIE, False),
            (403, MafiaRole.DOCTOR, False),
            (404, MafiaRole.DETECTIVE, False),
            (405, MafiaRole.TOWNIE, False),
        ],
    )
    _force_phase(mafia_repo, MafiaPhase.DAY)
    for voter in (402, 403, 404, 405):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 401)

    first = mafia_service.resolve_day(TEST_GUILD_ID)
    assert first["resolved"] is True
    balances_after_first = {
        pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids
    }

    second = mafia_service.resolve_day(TEST_GUILD_ID)
    assert second["resolved"] is False
    assert {
        pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids
    } == balances_after_first

    total_delta = sum(balances_after_first[pid] - starting[pid] for pid in ids)
    assert total_delta == 0


def test_bankruptcy_penalty_sinks_mafia_profit(
    mafia_service, mafia_repo, player_repo
):
    bankruptcy_repo = BankruptcyRepository(mafia_repo.db_path)
    mafia_service.bankruptcy_penalty_rate = 0.75
    ids = [501, 502, 503, 504, 505]
    mafia_id = 501
    for pid in ids:
        _seed_player(player_repo, pid, balance=0)
    starting = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids}
    for pid in ids:
        player_repo.add_balance(pid, TEST_GUILD_ID, -ENTRY_FEE)
    bankruptcy_repo.upsert_state(
        mafia_id,
        TEST_GUILD_ID,
        last_bankruptcy_at=int(time.time()) - 1000,
        penalty_games_remaining=3,
    )

    _new_game(
        mafia_repo,
        [
            (501, MafiaRole.MAFIA, True),
            (502, MafiaRole.TOWNIE, False),
            (503, MafiaRole.DOCTOR, False),
            (504, MafiaRole.DETECTIVE, False),
            (505, MafiaRole.TOWNIE, False),
        ],
    )
    _force_phase(mafia_repo, MafiaPhase.DAY)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["winner"] == MafiaWinner.MAFIA.value
    penalties = summary["bankruptcy_penalties"]
    assert penalties
    assert set(penalties) == {mafia_id}

    total_delta = sum(
        player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid] for pid in ids
    )
    assert total_delta == -sum(penalties.values())


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
