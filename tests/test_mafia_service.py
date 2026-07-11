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
    BOOKIE_PAYOUT,
    ENTRY_FEE,
    MAX_CYCLES,
    MAX_WINNER_PAYOUT,
    MVP_BONUS,
    ROLE_TABLE,
    MafiaService,
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
    result = mafia_service.start_game(TEST_GUILD_ID, force=True)
    assert result is None
    assert mafia_repo.get_game_for_date(TEST_GUILD_ID, "2026-04-24") is None


def test_idempotent_start(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, list(range(101, 110)))  # 9 players
    first = mafia_service.start_game(TEST_GUILD_ID, force=True)
    assert first is not None
    second = mafia_service.start_game(TEST_GUILD_ID, force=True)
    assert second is not None
    assert first.game_id == second.game_id


def test_start_daily_game_debits_entry_fee_once(mafia_repo, mafia_service, player_repo):
    ids = list(range(101, 110))
    _seed_eligible_via_dig(mafia_repo, ids)

    first = mafia_service.start_game(TEST_GUILD_ID, force=True)
    assert first is not None
    balances_after_first = {
        pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids
    }
    assert set(balances_after_first.values()) == {100 - ENTRY_FEE}

    second = mafia_service.start_game(TEST_GUILD_ID, force=True)
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
    # Past the debt floor: balance − ENTRY_FEE would drop below −MAX_DEBT (500).
    player_repo.update_balance(106, TEST_GUILD_ID, -495)

    game = mafia_service.start_game(TEST_GUILD_ID, force=True)
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
    game = mafia_service.start_game(TEST_GUILD_ID, force=True)
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
    # Jester and Bookie are optional swap-ins that each replace a townie.
    assert (
        counts[MafiaRole.TOWNIE]
        + counts[MafiaRole.JESTER]
        + counts[MafiaRole.BOOKIE]
        == townie_pool
    )


def test_godfather_always_among_mafia(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, list(range(101, 113)))  # 12 players → 3 mafia
    game = mafia_service.start_game(TEST_GUILD_ID, force=True)
    players = mafia_repo.get_players(game.game_id)
    gfs = [p for p in players if p.is_godfather]
    assert len(gfs) == 1
    assert gfs[0].role == MafiaRole.MAFIA


def test_oversized_roster_capped(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, list(range(101, 131)))  # 30 players
    game = mafia_service.start_game(TEST_GUILD_ID, force=True)
    assert game.roster_size == 15


def test_optout_excludes_player(mafia_repo, mafia_service):
    ids = list(range(101, 110))
    _seed_eligible_via_dig(mafia_repo, ids)
    mafia_service.set_optout(TEST_GUILD_ID, 105, True)
    game = mafia_service.start_game(TEST_GUILD_ID, force=True)
    rostered = {p.discord_id for p in mafia_repo.get_players(game.game_id)}
    assert 105 not in rostered


def test_guild_isolation(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, list(range(101, 110)))  # guild A
    game_a = mafia_service.start_game(TEST_GUILD_ID, force=True)
    assert game_a is not None
    # Guild B has no eligible players → no game
    game_b = mafia_service.start_game(TEST_GUILD_ID_SECONDARY, force=True)
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
    # Two detectives so each spends its single read on a distinct target:
    # the godfather reads as Town, a regular mafioso reads as Mafia.
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.DETECTIVE, False),
            (2, MafiaRole.MAFIA, True),  # Godfather
            (3, MafiaRole.MAFIA, False),
            (4, MafiaRole.DETECTIVE, False),
        ],
    )
    on_gf = mafia_service.submit_night_action(TEST_GUILD_ID, 1, 2, MafiaActionType.INVESTIGATE)
    assert on_gf["result"] == "Town"

    on_regular = mafia_service.submit_night_action(TEST_GUILD_ID, 4, 3, MafiaActionType.INVESTIGATE)
    assert on_regular["result"] == "Mafia"


def test_detective_one_read_per_night(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.DETECTIVE, False),
            (2, MafiaRole.MAFIA, True),
            (3, MafiaRole.MAFIA, False),
            (4, MafiaRole.TOWNIE, False),
        ],
    )
    first = mafia_service.submit_night_action(TEST_GUILD_ID, 1, 2, MafiaActionType.INVESTIGATE)
    assert first["ok"] and first["result"] == "Town"

    # A second, different target is rejected — no new info leaks.
    second = mafia_service.submit_night_action(TEST_GUILD_ID, 1, 3, MafiaActionType.INVESTIGATE)
    assert second["ok"] is False

    # Re-checking the original target replays the cached verdict.
    repeat = mafia_service.submit_night_action(TEST_GUILD_ID, 1, 2, MafiaActionType.INVESTIGATE)
    assert repeat["ok"] and repeat["result"] == "Town"

    # The stored read is still the first target, not the rejected one.
    stored = mafia_repo.get_action_for_actor(
        mafia_repo.get_active_game(TEST_GUILD_ID).game_id, 1, MafiaActionType.INVESTIGATE
    )
    assert stored["target_id"] == 2


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
    # Roster of 7 → pot = 210. resolve_day only redistributes the pot; it does
    # not debit fees (that happens in start_daily_game). To check post-resolution
    # net delta, we simulate the fee debit ourselves so this test exercises the
    # full end-to-end balance change.
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
    assert summary["pot_total"] == pot
    assert summary["entry_fee"] == ENTRY_FEE

    # Town has 6 winners. MVP gets base + MVP_BONUS + dust; others get base.
    # At a 15 buy-in none of these exceed the +50 cap, so there's no overflow.
    base = (pot - MVP_BONUS) // 6
    dust = (pot - MVP_BONUS) - base * 6
    assert summary["payout_per_winner"] == base
    assert summary["nonprofit_overflow"] == 0

    mvp_id = summary["mvp_id"]
    for pid in [102, 103, 104, 105, 106, 107]:
        delta = player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid]
        if pid == mvp_id:
            assert delta == base + MVP_BONUS + dust - ENTRY_FEE
        else:
            assert delta == base - ENTRY_FEE
        # No individual winner exceeds the cap.
        assert delta + ENTRY_FEE <= MAX_WINNER_PAYOUT

    # Mafia paid the fee and got nothing back.
    assert (
        player_repo.get_balance(101, TEST_GUILD_ID) - starting[101] == -ENTRY_FEE
    )

    # Zero-sum including the nonprofit sink: player deltas + overflow == 0.
    total_delta = sum(
        player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid] for pid in ids
    )
    assert total_delta + summary["nonprofit_overflow"] == 0


def test_payout_cap_overflow(mafia_service):
    # A small winning faction in a big roster blows past the +50/winner cap.
    # pot = 15 buy-in × 15 roster = 225; 4 mafia winners → 51 each before the
    # cap, so each clamps to 50 and the rest overflows to the nonprofit fund.
    pot_total = 15 * 15
    winning_ids = [1, 2, 3, 4]
    deltas, _, overflow = mafia_service._compute_payout_deltas(
        pot_total, winning_ids, mvp_id=1, bookie_id=None
    )
    assert all(v <= MAX_WINNER_PAYOUT for v in deltas.values())
    assert max(deltas.values()) == MAX_WINNER_PAYOUT
    assert sum(deltas.values()) + overflow == pot_total
    assert overflow > 0


def test_mvp_bonus_clamped_to_small_faction_pot(mafia_service):
    # Bookie takes the 50-coin cap off a 60 pot, leaving only 10 for the faction.
    # The MVP bonus must clamp to that 10 (not a flat MVP_BONUS=20), and that
    # clamped value — deltas[mvp] - payout_per_winner — is exactly what the
    # resolution embed now reports, instead of overstating a +20 never paid.
    deltas, payout_per_winner, overflow = mafia_service._compute_payout_deltas(
        pot_total=60, winning_ids=[1, 2, 3], mvp_id=1, bookie_id=9
    )
    mvp_extra = deltas[1] - payout_per_winner
    assert mvp_extra == 10
    assert mvp_extra < MVP_BONUS  # clamped, so the embed must not show +MVP_BONUS
    assert sum(deltas.values()) + overflow == 60  # and nothing was minted


# ── Multi-cycle engine (continuous cadence) ───────────────────────────────


def _undecided_roster():
    # 1 mafia vs 4 town: lynching nobody leaves the game undecided.
    return [
        (1, MafiaRole.MAFIA, False),
        (2, MafiaRole.TOWNIE, False),
        (3, MafiaRole.DETECTIVE, False),
        (4, MafiaRole.DOCTOR, False),
        (5, MafiaRole.TOWNIE, False),
    ]


def _set_day_number(mafia_repo, day_number: int) -> None:
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET day_number = ? "
            "WHERE game_id = (SELECT MAX(game_id) FROM mafia_games)",
            (day_number,),
        )


def test_multi_cycle_continues_when_undecided(mafia_service, mafia_repo):
    gid = _new_game(mafia_repo, _undecided_roster())
    # Night 1: mafia kills a townie.
    mafia_service.submit_night_action(TEST_GUILD_ID, 1, 4, MafiaActionType.KILL)
    mafia_service.resolve_night(TEST_GUILD_ID)
    g = mafia_repo.get_game_by_id(gid)
    assert g.phase == MafiaPhase.DAY and g.day_number == 1
    # Day 1: no lynch → still 1 mafia vs survivors → undecided → roll to night 2.
    rd = mafia_service.resolve_day(TEST_GUILD_ID)
    assert rd["resolved"] and rd["continued"] is True
    g = mafia_repo.get_game_by_id(gid)
    assert g.phase == MafiaPhase.NIGHT and g.day_number == 2 and g.status == "ACTIVE"


def test_cycle_cap_forces_tally(mafia_service, mafia_repo):
    _new_game(mafia_repo, _undecided_roster())
    _set_day_number(mafia_repo, MAX_CYCLES)
    _force_phase(mafia_repo, MafiaPhase.DAY)
    # No lynch, but we've hit MAX_CYCLES → forced standing tally. Town still
    # outnumbers the lone mafia, so town wins.
    rd = mafia_service.resolve_day(TEST_GUILD_ID)
    assert rd["resolved"] and rd["continued"] is False
    assert rd["winner"] == MafiaWinner.TOWN.value


def test_new_game_starts_after_previous_resolves(mafia_repo, mafia_service):
    _seed_eligible_via_dig(mafia_repo, list(range(101, 110)))
    g1 = mafia_service.start_game(TEST_GUILD_ID, force=True)
    assert g1 is not None
    # While g1 is active, start_game is idempotent.
    assert mafia_service.start_game(TEST_GUILD_ID, force=True).game_id == g1.game_id
    # Resolve g1, then a brand-new game starts immediately — sharing the same
    # start date, which the dropped UNIQUE(guild_id, game_date) now permits.
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET phase = ? WHERE game_id = ?",
            (MafiaPhase.RESOLVED.value, g1.game_id),
        )
    g2 = mafia_service.start_game(TEST_GUILD_ID, force=True)
    assert g2 is not None and g2.game_id != g1.game_id
    assert g2.game_date == g1.game_date


def test_phase_ready_gating(mafia_service, mafia_repo):
    _new_game(
        mafia_repo,
        [
            (1, MafiaRole.MAFIA, False),
            (2, MafiaRole.DOCTOR, False),
            (3, MafiaRole.DETECTIVE, False),
            (4, MafiaRole.TOWNIE, False),
            (5, MafiaRole.TOWNIE, False),
        ],
    )
    g = mafia_repo.get_active_game(TEST_GUILD_ID)
    # Night: not ready until mafia, doctor, and detective have all acted.
    assert mafia_service.night_ready(g) is False
    mafia_service.submit_night_action(TEST_GUILD_ID, 1, 4, MafiaActionType.KILL)
    mafia_service.submit_night_action(TEST_GUILD_ID, 2, 1, MafiaActionType.SAVE)
    assert mafia_service.night_ready(g) is False
    mafia_service.submit_night_action(TEST_GUILD_ID, 3, 1, MafiaActionType.INVESTIGATE)
    assert mafia_service.night_ready(g) is True

    # Day: not ready until every living player has voted.
    _force_phase(mafia_repo, MafiaPhase.DAY)
    g = mafia_repo.get_active_game(TEST_GUILD_ID)
    assert mafia_service.day_ready(g) is False
    for voter in (1, 2, 3, 4, 5):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 1)
    assert mafia_service.day_ready(g) is True


def test_town_bounty_pays_on_correct_lynch(mafia_service, mafia_repo, player_repo):
    ids = [1, 2, 3, 4, 5]
    for pid in ids:
        _seed_player(player_repo, pid, balance=50)
    _new_game(mafia_repo, _undecided_roster())
    _force_phase(mafia_repo, MafiaPhase.DAY)
    # Two townies stake a bounty on the mafioso (1 JC each, parked in nonprofit).
    assert mafia_service.submit_bounty(TEST_GUILD_ID, 2, 1)["ok"]
    assert mafia_service.submit_bounty(TEST_GUILD_ID, 3, 1)["ok"]
    # A second stake on the same target the same day is rejected.
    assert mafia_service.submit_bounty(TEST_GUILD_ID, 2, 1)["ok"] is False
    assert player_repo.get_balance(2, TEST_GUILD_ID) == 49

    for voter in (2, 3, 5):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 1)
    rd = mafia_service.resolve_day(TEST_GUILD_ID)
    assert rd["winner"] == MafiaWinner.TOWN.value
    bounty = rd["bounty"]
    assert bounty["reward"] > 0
    assert set(bounty["paid"]) == {2, 3}


def _nonprofit_fund(mafia_repo) -> int:
    with mafia_repo.connection() as conn:
        row = conn.cursor().execute(
            "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
            (TEST_GUILD_ID,),
        ).fetchone()
        return row["total_collected"] if row else 0


def test_bounty_not_paid_when_finalize_loses_race(
    mafia_service, mafia_repo, player_repo, monkeypatch
):
    """The Town Bounty draws from the nonprofit fund, so it must fire only after
    the day resolution is atomically claimed. If finalize loses the race (the
    game was resolved out-of-band, e.g. an admin stop racing the 5-min phase
    loop), the fund must not be debited and no contributor paid — otherwise the
    fund is double-spent for a day that reports unresolved."""
    ids = [1, 2, 3, 4, 5]
    for pid in ids:
        _seed_player(player_repo, pid, balance=50)
    _new_game(mafia_repo, _undecided_roster())  # 1 mafia (id 1) vs 4 town
    _force_phase(mafia_repo, MafiaPhase.DAY)
    assert mafia_service.submit_bounty(TEST_GUILD_ID, 2, 1)["ok"]
    assert mafia_service.submit_bounty(TEST_GUILD_ID, 3, 1)["ok"]
    for voter in (2, 3, 5):  # lynch the mafioso → town wins → finalize branch
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 1)

    fund_before = _nonprofit_fund(mafia_repo)
    assert fund_before > 0  # the two parked stakes
    bal_before = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in (2, 3)}

    # Simulate another path winning the resolution claim.
    monkeypatch.setattr(
        mafia_repo,
        "finalize_day_resolution",
        lambda **kw: {"applied": False, "reason": "already_resolved"},
    )
    rd = mafia_service.resolve_day(TEST_GUILD_ID)

    assert rd["resolved"] is False
    assert _nonprofit_fund(mafia_repo) == fund_before  # fund untouched
    for pid in (2, 3):
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == bal_before[pid]


def test_bounty_not_paid_when_advance_loses_race(
    mafia_service, mafia_repo, player_repo, monkeypatch
):
    """Same gating on the continue branch: lynching a mafioso while the game is
    still undecided must not pay the bounty if advance_to_next_cycle loses the
    resolution claim."""
    roster = [
        (1, MafiaRole.MAFIA, True),
        (6, MafiaRole.MAFIA, False),
        (2, MafiaRole.TOWNIE, False),
        (3, MafiaRole.DETECTIVE, False),
        (4, MafiaRole.DOCTOR, False),
        (5, MafiaRole.TOWNIE, False),
    ]
    for pid, _, _ in roster:
        _seed_player(player_repo, pid, balance=50)
    _new_game(mafia_repo, roster)  # 2 mafia vs 4 town → still undecided after 1 lynch
    _force_phase(mafia_repo, MafiaPhase.DAY)
    assert mafia_service.submit_bounty(TEST_GUILD_ID, 2, 1)["ok"]
    assert mafia_service.submit_bounty(TEST_GUILD_ID, 3, 1)["ok"]
    for voter in (2, 3, 5):  # lynch one mafioso; the other survives → continue
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 1)

    fund_before = _nonprofit_fund(mafia_repo)
    assert fund_before > 0
    bal_before = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in (2, 3)}

    monkeypatch.setattr(mafia_repo, "advance_to_next_cycle", lambda *a, **k: False)
    rd = mafia_service.resolve_day(TEST_GUILD_ID)

    assert rd["resolved"] is False
    assert _nonprofit_fund(mafia_repo) == fund_before
    for pid in (2, 3):
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == bal_before[pid]


def test_pot_for_roster_helper():
    assert _pot_for_roster(5) == 5 * ENTRY_FEE
    assert _pot_for_roster(10) == 10 * ENTRY_FEE
    assert _pot_for_roster(15) == 15 * ENTRY_FEE


def test_zero_sum_across_outcomes(mafia_service, mafia_repo, player_repo):
    """The pot mechanic conserves JC for every win outcome — net delta is 0."""
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

        # Zero-sum including the nonprofit sink: any pot beyond the +50/winner
        # cap leaves the player pool for the nonprofit fund, so player deltas
        # plus the overflow must net to zero.
        total_delta = sum(
            player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid] for pid in ids
        )
        assert total_delta + summary["nonprofit_overflow"] == 0, (
            f"scenario {i} ({expected_winner.value}): "
            f"net delta {total_delta}, overflow {summary['nonprofit_overflow']}"
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
    assert total_delta + first["nonprofit_overflow"] == 0


class _ForceSwapRng(random.Random):
    """RNG whose random() always returns 0.0 so every probability gate fires."""

    def random(self):  # noqa: D401 - deterministic override
        return 0.0


def test_bookie_assignment_swap_and_guard(mafia_service, mafia_repo):
    # Roster 8 has 4 townies → both the jester and bookie swaps fit.
    mafia_service._rng = _ForceSwapRng(0)
    players = mafia_service._assign_roles(0, TEST_GUILD_ID, list(range(1, 9)))
    roles = [p.role for p in players]
    assert roles.count(MafiaRole.BOOKIE) == 1
    assert roles.count(MafiaRole.JESTER) == 1

    # Roster 5 has only 2 townies → the jester swap consumes one, leaving < 2,
    # so the bookie guard suppresses the swap.
    mafia_service._rng = _ForceSwapRng(0)
    small = mafia_service._assign_roles(0, TEST_GUILD_ID, list(range(1, 6)))
    small_roles = [p.role for p in small]
    assert small_roles.count(MafiaRole.JESTER) == 1
    assert small_roles.count(MafiaRole.BOOKIE) == 0


def _bookie_scenario(mafia_repo, player_repo, wager_target: int):
    ids = [601, 602, 603, 604, 605]
    for pid in ids:
        _seed_player(player_repo, pid, balance=0)
    _new_game(
        mafia_repo,
        [
            (601, MafiaRole.MAFIA, True),
            (602, MafiaRole.TOWNIE, False),
            (603, MafiaRole.TOWNIE, False),
            (604, MafiaRole.DOCTOR, False),
            (605, MafiaRole.BOOKIE, False),
        ],
    )
    return ids


def test_bookie_hit_cashes_out(mafia_service, mafia_repo, player_repo):
    ids = _bookie_scenario(mafia_repo, player_repo, wager_target=601)
    # Bookie wagers on the mafioso the town will lynch.
    res = mafia_service.submit_night_action(
        TEST_GUILD_ID, 605, 601, MafiaActionType.WAGER
    )
    assert res["ok"]

    _force_phase(mafia_repo, MafiaPhase.DAY)
    for voter in (602, 603, 604):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 601)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    # The faction winner is unchanged — the Bookie is a parasitic side-winner.
    assert summary["winner"] == MafiaWinner.TOWN.value
    assert summary["bookie_id"] == 605
    # The skim is capped at the pot, so on a small (5×ENTRY_FEE) pot it can be
    # the whole pot rather than the full BOOKIE_PAYOUT.
    pot = _pot_for_roster(5)
    expected_skim = min(BOOKIE_PAYOUT, pot)
    assert summary["bookie_payout"] == expected_skim
    assert player_repo.get_balance(605, TEST_GUILD_ID) == expected_skim
    # No coins are minted: every collected coin ends up with a player or in the
    # nonprofit overflow, so player balances sum back to exactly the pot.
    assert summary["nonprofit_overflow"] >= 0
    assert sum(player_repo.get_balance(p, TEST_GUILD_ID) for p in ids) == pot


def test_bookie_miss_pays_nothing(mafia_service, mafia_repo, player_repo):
    _bookie_scenario(mafia_repo, player_repo, wager_target=602)
    # Bookie wagers on a townie who will NOT be lynched.
    mafia_service.submit_night_action(TEST_GUILD_ID, 605, 602, MafiaActionType.WAGER)

    _force_phase(mafia_repo, MafiaPhase.DAY)
    for voter in (602, 603, 604):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 601)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["winner"] == MafiaWinner.TOWN.value
    assert summary["bookie_id"] is None
    assert summary["bookie_payout"] == 0
    # The Bookie took the entry-fee risk and got nothing back.
    assert player_repo.get_balance(605, TEST_GUILD_ID) == 0


def test_bookie_hit_not_recorded_when_resolution_loses_race(
    mafia_service, mafia_repo, player_repo, monkeypatch
):
    """The Bookie HIT is durable state that feeds the end-of-week skim, so like
    the Town Bounty it must be committed only AFTER the day resolution is
    claimed. If resolve_day loses the race (an admin stop/abort resolved the day
    out-of-band), no spurious HIT may be left on record — otherwise a rival
    finalize pays the Bookie a skim for a lynch that was never enacted."""
    _bookie_scenario(mafia_repo, player_repo, wager_target=601)
    assert mafia_service.submit_night_action(
        TEST_GUILD_ID, 605, 601, MafiaActionType.WAGER
    )["ok"]
    _force_phase(mafia_repo, MafiaPhase.DAY)
    for voter in (602, 603, 604):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 601)

    game = mafia_repo.get_active_game(TEST_GUILD_ID)
    dn = game.day_number

    # Lose the resolution claim (another path resolved the day first).
    monkeypatch.setattr(
        mafia_repo,
        "finalize_day_resolution",
        lambda **kw: {"applied": False, "reason": "already_resolved"},
    )
    # Spy on the durable-HIT writer: the deterministic, load-insensitive core
    # guarantee is that _commit_post_claim (its only caller during resolution) is
    # never reached when the claim is lost — independent of re-reading DB state.
    marked: list = []
    real_mark = mafia_service._mark_bookie_wager
    monkeypatch.setattr(
        mafia_service,
        "_mark_bookie_wager",
        lambda *a, **k: marked.append(a) or real_mark(*a, **k),
    )
    rd = mafia_service.resolve_day(TEST_GUILD_ID)
    assert rd["resolved"] is False
    assert marked == []  # the HIT writer was never invoked

    # And the observable outcome: no spurious HIT persisted, Bookie unpaid.
    wagers = mafia_repo.get_actions(
        game.game_id, MafiaActionType.WAGER, MafiaPhase.NIGHT, day_number=dn
    )
    assert all(w.get("result") != "HIT" for w in wagers)
    assert player_repo.get_balance(605, TEST_GUILD_ID) == 0

    # A real finalize that wins the claim (standing tally, no lynch enacted) must
    # not pay the Bookie a skim off a HIT that was never legitimately recorded.
    monkeypatch.undo()
    forced = mafia_service.force_finalize(TEST_GUILD_ID)
    assert forced["resolved"] is True
    assert forced["bookie_id"] is None
    assert player_repo.get_balance(605, TEST_GUILD_ID) == 0


def test_bookie_hit_carries_over_multi_day_to_finalize(
    mafia_service, mafia_repo, player_repo
):
    """The core multi-day mechanic the fix relies on: a correct Bookie wager on a
    CONTINUE day persists a durable HIT (committed after that day's claim), and a
    LATER finalize day cashes it out via the past-HIT count even though the
    Bookie placed no wager that final day (extra_hit is False then)."""
    roster = [
        (601, MafiaRole.MAFIA, True),
        (606, MafiaRole.MAFIA, False),
        (602, MafiaRole.TOWNIE, False),
        (603, MafiaRole.TOWNIE, False),
        (604, MafiaRole.DOCTOR, False),
        (605, MafiaRole.BOOKIE, False),
    ]
    for pid, _, _ in roster:
        _seed_player(player_repo, pid, balance=0)
    gid = _new_game(mafia_repo, roster)
    with mafia_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE mafia_games SET roster_size = ? WHERE game_id = ?", (len(roster), gid)
        )

    # Day 1 (NIGHT): the Bookie wagers on the mafioso the town will lynch.
    assert mafia_service.submit_night_action(
        TEST_GUILD_ID, 605, 601, MafiaActionType.WAGER
    )["ok"]
    _force_phase(mafia_repo, MafiaPhase.DAY)
    for voter in (602, 603, 604):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 601)
    rd1 = mafia_service.resolve_day(TEST_GUILD_ID)
    # One mafioso still alive → undecided → continue, no payout yet.
    assert rd1["continued"] is True
    # The day-1 HIT is now durably on record (persisted after the claim).
    day1_wagers = mafia_repo.get_actions(
        gid, MafiaActionType.WAGER, MafiaPhase.NIGHT, day_number=1
    )
    assert any(w["target_id"] == 601 and w.get("result") == "HIT" for w in day1_wagers)

    # Day 2 (DAY): the Bookie does NOT wager again. Town lynches the last
    # mafioso → TOWN finalize. The skim must be driven purely by the day-1 HIT
    # (this day's extra_hit is False), proving the carry-over count works.
    _force_phase(mafia_repo, MafiaPhase.DAY)
    for voter in (602, 603, 604):
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 606)
    rd2 = mafia_service.resolve_day(TEST_GUILD_ID)
    assert rd2["continued"] is False
    assert rd2["winner"] == MafiaWinner.TOWN.value
    assert rd2["bookie_id"] == 605
    assert rd2["bookie_payout"] > 0
    assert player_repo.get_balance(605, TEST_GUILD_ID) == rd2["bookie_payout"]


def test_bankruptcy_penalty_sinks_mafia_profit(
    mafia_service, mafia_repo, player_repo
):
    bankruptcy_repo = BankruptcyRepository(mafia_repo.db_path)
    mafia_service.bankruptcy_penalty_rate = 0.75
    ids = [501, 502, 503, 504, 505]
    town_ids = [502, 503, 504, 505]
    for pid in ids:
        _seed_player(player_repo, pid, balance=0)
    starting = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in ids}
    for pid in ids:
        player_repo.add_balance(pid, TEST_GUILD_ID, -ENTRY_FEE)
    for pid in town_ids:
        bankruptcy_repo.upsert_state(
            pid,
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
    for voter in town_ids:
        mafia_service.submit_day_vote(TEST_GUILD_ID, voter, 501)

    summary = mafia_service.resolve_day(TEST_GUILD_ID)
    assert summary["winner"] == MafiaWinner.TOWN.value
    penalties = summary["bankruptcy_penalties"]
    assert penalties
    assert set(penalties).issubset(set(town_ids))

    # Both sinks drain the player pool: bankruptcy penalties and the +50-cap
    # overflow to the nonprofit fund.
    total_delta = sum(
        player_repo.get_balance(pid, TEST_GUILD_ID) - starting[pid] for pid in ids
    )
    assert total_delta == -sum(penalties.values()) - summary["nonprofit_overflow"]


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
