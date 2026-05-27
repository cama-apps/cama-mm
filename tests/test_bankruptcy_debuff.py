"""Tests for the bankruptcy debuff applied across all payout sources.

After declaring bankruptcy a player keeps only ``BANKRUPTCY_PENALTY_RATE`` of
their winnings (currently 75% ⇒ 25% withheld) until they win
``BANKRUPTCY_PENALTY_GAMES`` inhouse games (currently 3). This used to apply only
to match rewards; these tests pin the broadened behavior across bet settlement,
/dig (active yield + boss, via the shared ``_penalize_jc`` helper), and
prediction resolution, plus the shared debit primitive and the configured knobs.

Why these matter: a penalized player's winnings must shrink by exactly the rate,
but their *stake* (bet, contract cost basis) must always be returned whole — so
winning never nets a loss. The penalty is a coin sink, consistent with the
pre-existing match-reward penalty.
"""

import random
import time

import pytest

from commands.trivia import _jc_for_streak
from config import (
    BANKRUPTCY_PENALTY_GAMES,
    BANKRUPTCY_PENALTY_RATE,
)
from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.bet_repository import BetRepository
from repositories.dig_repository import DigRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from repositories.prediction_repository import PredictionRepository
from services.bankruptcy_service import BankruptcyService
from services.betting_service import BettingService
from services.dig_service import DigService
from services.garnishment_service import GarnishmentService
from services.match_service import MatchService
from services.prediction_service import PredictionService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def repos(repo_db_path):
    return {
        "player": PlayerRepository(repo_db_path),
        "bankruptcy": BankruptcyRepository(repo_db_path),
        "bet": BetRepository(repo_db_path),
        "prediction": PredictionRepository(repo_db_path),
        "dig": DigRepository(repo_db_path),
        "match": MatchRepository(repo_db_path),
        "path": repo_db_path,
    }


@pytest.fixture
def bankruptcy_service(repos):
    """Uses config defaults (no overrides), so these tests also pin the
    configured BANKRUPTCY_PENALTY_GAMES / _RATE values."""
    return BankruptcyService(repos["bankruptcy"], repos["player"])


def _add_player(player_repo, pid, balance=3):
    player_repo.add(
        discord_id=pid, discord_username=f"P{pid}", guild_id=TEST_GUILD_ID,
        glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06,
    )
    if balance != 3:
        player_repo.update_balance(pid, TEST_GUILD_ID, balance)


def _penalize(repos, bankruptcy_service, pid):
    """Declare bankruptcy via the real path so a ``bankruptcy_state`` row exists
    with ``penalty_games_remaining == BANKRUPTCY_PENALTY_GAMES``. Leaves balance
    at ``BANKRUPTCY_FRESH_START_BALANCE``."""
    repos["player"].update_balance(pid, TEST_GUILD_ID, -50)
    res = bankruptcy_service.execute_bankruptcy(pid, TEST_GUILD_ID)
    assert res.success
    assert (
        bankruptcy_service.get_state(pid, TEST_GUILD_ID).penalty_games_remaining
        == BANKRUPTCY_PENALTY_GAMES
    )


def _expected_penalty(profit: int) -> int:
    """The withheld share for a given gross profit, per the configured rate.

    Floors the penalty (the withheld share), not the kept winnings, so a
    fractional rate never rounds a small payout all the way down to zero."""
    return int(profit * (1 - BANKRUPTCY_PENALTY_RATE))


# --------------------------------------------------------------------------- #
# Configured knobs
# --------------------------------------------------------------------------- #


class TestConstants:
    def test_bankruptcy_sets_three_penalty_games(self, repos, bankruptcy_service):
        _add_player(repos["player"], 5001)
        _penalize(repos, bankruptcy_service, 5001)
        assert (
            bankruptcy_service.get_state(5001, TEST_GUILD_ID).penalty_games_remaining
            == BANKRUPTCY_PENALTY_GAMES
        )

    def test_three_wins_clear_the_penalty(self, repos, bankruptcy_service):
        _add_player(repos["player"], 5002)
        _penalize(repos, bankruptcy_service, 5002)
        for _ in range(BANKRUPTCY_PENALTY_GAMES - 1):
            bankruptcy_service.on_game_won(5002, TEST_GUILD_ID)
        # Still penalized one win short of the threshold...
        assert (
            bankruptcy_service.get_state(5002, TEST_GUILD_ID).penalty_games_remaining == 1
        )
        bankruptcy_service.on_game_won(5002, TEST_GUILD_ID)
        # ...cleared on the third win.
        assert (
            bankruptcy_service.get_state(5002, TEST_GUILD_ID).penalty_games_remaining == 0
        )

    def test_rate_keeps_three_quarters(self, repos, bankruptcy_service):
        _add_player(repos["player"], 5003)
        _penalize(repos, bankruptcy_service, 5003)
        info = bankruptcy_service.apply_penalty_to_winnings(5003, 100, TEST_GUILD_ID)
        assert info["penalized"] == int(100 * BANKRUPTCY_PENALTY_RATE)
        assert info["penalty_applied"] == 25


# --------------------------------------------------------------------------- #
# Shared debit primitive (used by bet settlement + prediction resolution)
# --------------------------------------------------------------------------- #


class TestDebitPrimitive:
    def test_debits_penalty_share_of_profit(self, repos, bankruptcy_service):
        pid = 5101
        _add_player(repos["player"], pid)
        _penalize(repos, bankruptcy_service, pid)  # balance -> FRESH_START, penalized
        # Simulate the gross payout already credited by an atomic settlement.
        repos["player"].add_balance(pid, TEST_GUILD_ID, 100)
        before = repos["player"].get_balance(pid, TEST_GUILD_ID)

        penalties = bankruptcy_service.debit_bankruptcy_penalty([(pid, 100)], TEST_GUILD_ID)

        assert penalties == {pid: 25}
        assert repos["player"].get_balance(pid, TEST_GUILD_ID) == before - 25

    def test_no_debit_when_not_penalized(self, repos, bankruptcy_service):
        pid = 5102
        _add_player(repos["player"], pid, balance=100)
        penalties = bankruptcy_service.debit_bankruptcy_penalty([(pid, 100)], TEST_GUILD_ID)
        assert penalties == {}
        assert repos["player"].get_balance(pid, TEST_GUILD_ID) == 100

    def test_skips_nonpositive_profit(self, repos, bankruptcy_service):
        pid = 5103
        _add_player(repos["player"], pid)
        _penalize(repos, bankruptcy_service, pid)
        before = repos["player"].get_balance(pid, TEST_GUILD_ID)
        penalties = bankruptcy_service.debit_bankruptcy_penalty(
            [(pid, 0), (pid, -10)], TEST_GUILD_ID
        )
        assert penalties == {}
        assert repos["player"].get_balance(pid, TEST_GUILD_ID) == before


# --------------------------------------------------------------------------- #
# Dota bet settlement
# --------------------------------------------------------------------------- #


class TestBettingDebuff:
    def _betting_setup(self, repos, bankruptcy_service):
        player_repo = repos["player"]
        betting_service = BettingService(
            repos["bet"],
            player_repo,
            garnishment_service=GarnishmentService(player_repo),
            bankruptcy_service=bankruptcy_service,
        )
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=repos["match"],
            use_glicko=True,
            betting_service=betting_service,
        )
        for pid in range(1000, 1010):
            player_repo.add(
                discord_id=pid, discord_username=f"P{pid}", guild_id=TEST_GUILD_ID,
                initial_mmr=1500, glicko_rating=1500.0, glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        match_service.shuffle_players(
            list(range(1000, 1010)), guild_id=TEST_GUILD_ID, betting_mode="house"
        )
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        if pending.bet_lock_until is None or pending.bet_lock_until <= int(time.time()):
            pending.bet_lock_until = int(time.time()) + 600
        return betting_service, match_service, pending

    def test_penalized_winner_loses_quarter_of_profit_stake_protected(
        self, repos, bankruptcy_service
    ):
        player_repo = repos["player"]
        betting_service, _, pending = self._betting_setup(repos, bankruptcy_service)
        bettor = pending.radiant_team_ids[0]

        _penalize(repos, bankruptcy_service, bettor)  # balance -> 3, penalized
        player_repo.add_balance(bettor, TEST_GUILD_ID, 20)  # 23
        betting_service.place_bet(TEST_GUILD_ID, bettor, "radiant", 5, pending)  # -> 18

        dist = betting_service.settle_bets(123, TEST_GUILD_ID, "radiant", pending_state=pending)

        # House 1:1 -> payout 10 on a 5 stake => profit 5; withheld = int(5*0.25) = 1
        # (the penalty is floored, so it rounds down on this small profit).
        assert dist["bankruptcy_penalties"][bettor] == _expected_penalty(5)
        # 18 (post-bet) + 10 (payout) - 1 (debuff) = 27. Stake of 5 returned whole;
        # the win still nets a gain (27 > 23), never a loss.
        assert player_repo.get_balance(bettor, TEST_GUILD_ID) == 27

    def test_non_penalized_winner_keeps_full_payout(self, repos, bankruptcy_service):
        player_repo = repos["player"]
        betting_service, _, pending = self._betting_setup(repos, bankruptcy_service)
        bettor = pending.radiant_team_ids[1]
        player_repo.add_balance(bettor, TEST_GUILD_ID, 20)  # 3 + 20 = 23
        betting_service.place_bet(TEST_GUILD_ID, bettor, "radiant", 5, pending)  # -> 18

        dist = betting_service.settle_bets(124, TEST_GUILD_ID, "radiant", pending_state=pending)

        assert bettor not in dist.get("bankruptcy_penalties", {})
        assert player_repo.get_balance(bettor, TEST_GUILD_ID) == 28  # full payout


# --------------------------------------------------------------------------- #
# Prediction resolution
# --------------------------------------------------------------------------- #


class TestPredictionDebuff:
    def test_penalized_winner_profit_debuffed_basis_returned(
        self, repos, bankruptcy_service
    ):
        player_repo = repos["player"]
        prediction_service = PredictionService(
            prediction_repo=repos["prediction"],
            player_repo=player_repo,
            bankruptcy_service=bankruptcy_service,
        )
        pid = 6001
        _add_player(player_repo, pid)
        _penalize(repos, bankruptcy_service, pid)  # balance -> 3, penalized
        player_repo.update_balance(pid, TEST_GUILD_ID, 1000)  # fund buying

        market = prediction_service.create_orderbook_prediction(
            guild_id=TEST_GUILD_ID, creator_id=1, question="resolves yes?", initial_fair=50,
        )["prediction_id"]
        prediction_service.buy_contracts(
            prediction_id=market, discord_id=pid, side="yes", contracts=5
        )
        pre_resolution = player_repo.get_balance(pid, TEST_GUILD_ID)

        result = prediction_service.resolve_orderbook(prediction_id=market, outcome="yes")

        winner = next(w for w in result["winners"] if w["discord_id"] == pid)
        penalty = winner["bankruptcy_penalty"]
        # Reconstruct the pre-debuff profit and confirm the rate was applied.
        gross_profit = winner["profit"] + penalty
        assert gross_profit > 0  # winning YES contracts profit over cost basis
        assert penalty == _expected_penalty(gross_profit)
        # Net credited = gross payout - penalty; balance rose by the net payout.
        gain = player_repo.get_balance(pid, TEST_GUILD_ID) - pre_resolution
        assert gain == winner["payout"]
        # Stake protected: they still profited (gain exceeds nothing-lost), never a loss.
        assert gain > 0


# --------------------------------------------------------------------------- #
# /dig — active yield (live path) + the shared _penalize_jc helper (also used
# by the boss-victory path in combat_mixin)
# --------------------------------------------------------------------------- #


class TestDigDebuff:
    @pytest.fixture
    def dig_service(self, repos, bankruptcy_service, monkeypatch):
        svc = DigService(
            repos["dig"],
            repos["player"],
            bankruptcy_repo=repos["bankruptcy"],
            bankruptcy_service=bankruptcy_service,
        )
        # Neutralize weather so random rolls don't perturb yields.
        monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
        return svc

    def test_penalize_jc_helper(self, repos, bankruptcy_service, dig_service):
        """The helper both dig() and the boss path call: reduces yield for a
        penalized digger, leaves it untouched otherwise."""
        pen = 7001
        plain = 7002
        _add_player(repos["player"], pen)
        _add_player(repos["player"], plain)
        _penalize(repos, bankruptcy_service, pen)

        assert dig_service._penalize_jc(pen, TEST_GUILD_ID, 100) == (75, 25)
        assert dig_service._penalize_jc(plain, TEST_GUILD_ID, 100) == (100, 0)
        # Non-positive yield is never penalized.
        assert dig_service._penalize_jc(pen, TEST_GUILD_ID, 0) == (0, 0)

    def test_normal_dig_routes_yield_through_debuff(
        self, repos, bankruptcy_service, dig_service, monkeypatch
    ):
        """The live dig() path runs its yield through the bankruptcy helper, and
        the one-time welcome dig is intentionally exempt. Deterministic: a normal
        dig can legitimately roll 0 JC, so we assert the routing (and the rate
        share only when the dig actually yielded), not a fixed amount."""
        pid = 7101
        _add_player(repos["player"], pid, balance=100)
        _penalize(repos, bankruptcy_service, pid)

        seen: list[int] = []
        orig = dig_service._penalize_jc

        def _spy(discord_id, guild_id, amount):
            if discord_id == pid:
                seen.append(amount)
            return orig(discord_id, guild_id, amount)

        monkeypatch.setattr(dig_service, "_penalize_jc", _spy)

        # Welcome dig creates the tunnel and is NOT debuffed.
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        random.seed(99)
        dig_service.dig(pid, TEST_GUILD_ID)
        assert seen == [], "welcome dig should not be debuffed"

        # A normal dig past cooldown, forced clear of cave-in / event.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        random.seed(7)
        r = dig_service.dig(pid, TEST_GUILD_ID)

        assert r["success"] and not r.get("cave_in")
        assert seen, "normal dig did not route its yield through the bankruptcy debuff"
        gross = r["jc_earned"] + r["bankruptcy_penalty"]
        if gross > 0:
            assert r["jc_earned"] == gross - _expected_penalty(gross)
            assert r["bankruptcy_penalty"] == gross - r["jc_earned"]

    def test_boss_victory_jc_debuffed(
        self, repos, bankruptcy_service, dig_service, monkeypatch
    ):
        """The live boss path (start_boss_duel -> _resolve_duel_outcome) debuffs
        the victory payout. Existing boss tests use the legacy ``fight_boss``
        path and wire no bankruptcy_service, so this is the only guard on the
        live boss debuff branch."""
        import json

        pid = 7301
        repos["player"].add(
            discord_id=pid, discord_username="boss", guild_id=TEST_GUILD_ID,
            glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06,
        )
        # Place one block before the depth-25 boss boundary (mirrors the boss
        # test helpers): a welcome dig to create the tunnel, then jump to depth.
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(pid, TEST_GUILD_ID)
        repos["dig"].update_tunnel(
            pid, TEST_GUILD_ID, depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
        _penalize(repos, bankruptcy_service, pid)  # penalized; balance -> FRESH_START
        repos["player"].update_balance(pid, TEST_GUILD_ID, 1000)  # fund the wager

        # Force a clean win, then resolve (auto-resolve, or finish a paused duel).
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.01)
        result = dig_service.start_boss_duel(pid, TEST_GUILD_ID, "cautious", wager=10)
        if result.get("pending_prompt"):
            result = dig_service.resume_boss_duel(pid, TEST_GUILD_ID, option_idx=0)

        assert result["success"] and result.get("won") is True
        penalty = result["bankruptcy_penalty"]
        assert penalty > 0  # boss winnings were debuffed
        gross = result["payout"] + penalty
        assert result["payout"] == gross - _expected_penalty(gross)


# --------------------------------------------------------------------------- #
# /trivia — milestone payouts (debuff applied in commands/trivia.py)
# --------------------------------------------------------------------------- #


class TestTriviaDebuff:
    """Trivia milestone payouts are debuffed for a bankrupt player.

    The trivia view (``commands/trivia.py:_handle_answer``) runs its milestone JC
    through ``apply_penalty_to_winnings`` as the last step before crediting. That
    payout lives inside a Discord View and isn't unit-invokable, so — mirroring
    the trivia coverage in ``test_bankruptcy_buffs.py`` — we pin the building
    blocks the view composes: the buffed milestone schedule and the per-source
    debuff applied to it. Trivia must NOT clear the penalty; only inhouse match
    wins do.
    """

    def test_hard_tier_milestone_buffed_to_two(self):
        # Buff: the hard tier (streak 10) now pays +2; everything else unchanged.
        assert _jc_for_streak(3) == 1
        assert _jc_for_streak(6) == 1
        assert _jc_for_streak(10) == 2
        assert _jc_for_streak(14) == 1  # challenging cadence (+1 every 4) intact
        assert _jc_for_streak(7) == 0  # non-milestone streaks award nothing

    def test_penalized_trivia_milestone_is_not_zeroed_by_rounding(self, repos, bankruptcy_service):
        pid = 8001
        _add_player(repos["player"], pid)
        _penalize(repos, bankruptcy_service, pid)

        # Milestone payouts are tiny (1-2 JC). The debuff floors the *withheld*
        # share (int(amount * 0.25)), not the kept amount, so on these small
        # payouts the penalty rounds to 0 and the bankrupt player keeps the whole
        # milestone — instead of having a 1-JC payout zeroed out entirely.
        assert _jc_for_streak(10) == 2
        assert bankruptcy_service.apply_penalty_to_winnings(pid, 2, TEST_GUILD_ID)["penalized"] == 2
        assert bankruptcy_service.apply_penalty_to_winnings(pid, 1, TEST_GUILD_ID)["penalized"] == 1

    def test_non_penalized_trivia_keeps_full_milestone(self, repos, bankruptcy_service):
        pid = 8002
        _add_player(repos["player"], pid, balance=100)
        jc = _jc_for_streak(10)
        info = bankruptcy_service.apply_penalty_to_winnings(pid, jc, TEST_GUILD_ID)
        assert info["penalized"] == jc  # no bankruptcy declared -> full payout
        assert info["penalty_applied"] == 0

    def test_trivia_does_not_clear_penalty(self, repos, bankruptcy_service):
        """Awarding debuffed trivia JC must not decrement the penalty counter —
        only inhouse match wins clear bankruptcy. The trivia path uses the
        read-only ``apply_penalty_to_winnings`` primitive, never ``on_game_won``."""
        pid = 8003
        _add_player(repos["player"], pid)
        _penalize(repos, bankruptcy_service, pid)

        before = bankruptcy_service.get_state(pid, TEST_GUILD_ID).penalty_games_remaining
        # Apply the trivia debuff several times (as multiple milestones would).
        for _ in range(3):
            bankruptcy_service.apply_penalty_to_winnings(pid, _jc_for_streak(10), TEST_GUILD_ID)
        after = bankruptcy_service.get_state(pid, TEST_GUILD_ID).penalty_games_remaining
        assert after == before == BANKRUPTCY_PENALTY_GAMES
