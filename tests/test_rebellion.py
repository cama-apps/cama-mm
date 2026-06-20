"""
Tests for the Wheel War (Rebellion) feature.

Covers:
- Schema migration (wheel_wars + war_bets tables)
- RebellionRepository CRUD
- RebellionService business logic:
  - Eligibility check
  - Veteran vote weighting
  - Threshold formula
  - Quorum check
  - Fizzle path
  - Attacker win path
  - Defender win path
  - War spin consumption
  - Celebration spin mechanics
  - RETRIBUTION logic
  - Meta-bet parimutuel payouts
"""

import json
import time

import pytest

from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.player_repository import PlayerRepository
from repositories.rebellion_repository import RebellionRepository
from services.rebellion_service import RebellionService
from tests.conftest import TEST_GUILD_ID

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def rebellion_repo(repo_db_path):
    return RebellionRepository(repo_db_path)


@pytest.fixture
def player_repo(repo_db_path):
    return PlayerRepository(repo_db_path)


@pytest.fixture
def bankruptcy_repo(repo_db_path):
    return BankruptcyRepository(repo_db_path)


@pytest.fixture
def rebellion_service(rebellion_repo, bankruptcy_repo, player_repo):
    return RebellionService(
        rebellion_repo=rebellion_repo,
        bankruptcy_repo=bankruptcy_repo,
        player_repo=player_repo,
    )


def _add_player(player_repo, discord_id: int, balance: int = 100, guild_id: int = TEST_GUILD_ID):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"player_{discord_id}",
        guild_id=guild_id,
    )
    player_repo.update_balance(discord_id, guild_id, balance)
    return discord_id


def _set_bankrupt(bankruptcy_repo, discord_id: int, guild_id: int = TEST_GUILD_ID, bankruptcy_count: int = 1, penalty_games: int = 3):
    """Set up bankruptcy state for a player."""
    now = int(time.time())
    bankruptcy_repo.upsert_state(
        discord_id=discord_id,
        guild_id=guild_id,
        last_bankruptcy_at=now - 3600,  # 1 hour ago
        penalty_games_remaining=penalty_games,
    )
    # Adjust bankruptcy_count (upsert_state increments it from 0)
    # If we need count=2, call upsert again
    for _ in range(bankruptcy_count - 1):
        bankruptcy_repo.upsert_state(
            discord_id=discord_id,
            guild_id=guild_id,
            last_bankruptcy_at=now - 3600,
            penalty_games_remaining=penalty_games,
        )


# ---------------------------------------------------------------------------
# Schema migration test
# ---------------------------------------------------------------------------


class TestSchemaMigration:
    def test_wheel_wars_table_exists(self, repo_db_path):
        import sqlite3
        conn = sqlite3.connect(repo_db_path)
        cursor = conn.cursor()
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='wheel_wars'")
        assert cursor.fetchone() is not None, "wheel_wars table should exist"
        conn.close()

    def test_war_bets_table_exists(self, repo_db_path):
        import sqlite3
        conn = sqlite3.connect(repo_db_path)
        cursor = conn.cursor()
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='war_bets'")
        assert cursor.fetchone() is not None, "war_bets table should exist"
        conn.close()

    def test_wheel_wars_columns(self, repo_db_path):
        import sqlite3
        conn = sqlite3.connect(repo_db_path)
        conn.row_factory = sqlite3.Row
        cursor = conn.cursor()
        cursor.execute("PRAGMA table_info(wheel_wars)")
        columns = {row["name"] for row in cursor.fetchall()}
        conn.close()
        required = {
            "war_id", "guild_id", "inciter_id", "status",
            "attack_voter_ids", "defend_voter_ids",
            "effective_attack_count", "effective_defend_count",
            "vote_closes_at", "battle_roll", "victory_threshold",
            "outcome", "wheel_effect_spins_remaining",
            "war_scar_wedge_label", "celebration_spins_used",
            "celebration_spin_expires_at", "created_at", "resolved_at",
        }
        assert required.issubset(columns)


# ---------------------------------------------------------------------------
# Eligibility tests
# ---------------------------------------------------------------------------


class TestEligibility:
    def test_ineligible_no_bankruptcy_history(self, rebellion_service, player_repo):
        _add_player(player_repo, 1001)
        result = rebellion_service.check_incite_eligibility(1001, TEST_GUILD_ID)
        assert not result["eligible"]

    def test_eligible_with_penalty_games(self, rebellion_service, player_repo, bankruptcy_repo):
        _add_player(player_repo, 1002)
        _set_bankrupt(bankruptcy_repo, 1002, penalty_games=3)
        result = rebellion_service.check_incite_eligibility(1002, TEST_GUILD_ID)
        assert result["eligible"]

    def test_eligible_with_recent_bankruptcy(self, rebellion_service, player_repo, bankruptcy_repo):
        _add_player(player_repo, 1003)
        _set_bankrupt(bankruptcy_repo, 1003, penalty_games=0)
        result = rebellion_service.check_incite_eligibility(1003, TEST_GUILD_ID)
        assert result["eligible"]

    def test_ineligible_old_bankruptcy_no_penalty(self, rebellion_service, player_repo, bankruptcy_repo):
        """Bankruptcy older than 7 days with no penalty games = not eligible."""
        _add_player(player_repo, 1004)
        bankruptcy_repo.upsert_state(
            discord_id=1004,
            guild_id=TEST_GUILD_ID,
            last_bankruptcy_at=int(time.time()) - 8 * 86400,  # 8 days ago
            penalty_games_remaining=0,
        )
        result = rebellion_service.check_incite_eligibility(1004, TEST_GUILD_ID)
        assert not result["eligible"]

    def test_ineligible_active_war_in_guild(self, rebellion_service, player_repo, bankruptcy_repo, rebellion_repo):
        """Can't incite if a war is already in progress."""
        _add_player(player_repo, 1005)
        _set_bankrupt(bankruptcy_repo, 1005, penalty_games=3)
        # Create an active war
        now = int(time.time())
        rebellion_repo.create_war(TEST_GUILD_ID, 1005, now + 900, now)
        result = rebellion_service.check_incite_eligibility(1005, TEST_GUILD_ID)
        assert not result["eligible"]
        assert "already in progress" in result["reason"]

    def test_ineligible_inciter_cooldown(self, rebellion_service, player_repo, bankruptcy_repo, rebellion_repo):
        """Can't incite again within 7-day cooldown."""
        _add_player(player_repo, 1006)
        _set_bankrupt(bankruptcy_repo, 1006, penalty_games=3)
        now = int(time.time())
        war_id = rebellion_repo.create_war(TEST_GUILD_ID, 1006, now - 800, now - 1000)
        rebellion_repo.set_fizzled(war_id, now - 900)
        rebellion_repo.set_inciter_cooldown(war_id, 1006, TEST_GUILD_ID, now + 86400)
        result = rebellion_service.check_incite_eligibility(1006, TEST_GUILD_ID)
        assert not result["eligible"]


# ---------------------------------------------------------------------------
# Veteran vote weighting
# ---------------------------------------------------------------------------


class TestVeteranVoteWeight:
    def test_normal_player_weight_1(self, rebellion_service, player_repo, bankruptcy_repo, rebellion_repo):
        _add_player(player_repo, 2001)
        _add_player(player_repo, 2002)
        _set_bankrupt(bankruptcy_repo, 2001, penalty_games=3)

        now = int(time.time())
        war_id = rebellion_repo.create_war(TEST_GUILD_ID, 2001, now + 900, now)

        # Player 2002 has only 1 bankruptcy
        _set_bankrupt(bankruptcy_repo, 2002, bankruptcy_count=1, penalty_games=0)
        result = rebellion_service.process_attack_vote(war_id, 2002, TEST_GUILD_ID)
        assert result["success"]
        assert not result.get("is_veteran")

        war = rebellion_repo.get_war(war_id)
        # inciter at 1.0 (or veteran weight) + 2002 at 1.0 = should be ~2.0
        assert war["effective_attack_count"] >= 2.0

    def test_veteran_player_weight_1_5(self, rebellion_service, player_repo, bankruptcy_repo, rebellion_repo):
        """Player with 2+ bankruptcies gets 1.5 effective votes."""
        _add_player(player_repo, 2003)
        _add_player(player_repo, 2004)
        _set_bankrupt(bankruptcy_repo, 2003, penalty_games=3)

        now = int(time.time())
        war_id = rebellion_repo.create_war(TEST_GUILD_ID, 2003, now + 900, now)

        # Player 2004 has 2 bankruptcies (veteran)
        _set_bankrupt(bankruptcy_repo, 2004, bankruptcy_count=2, penalty_games=0)
        result = rebellion_service.process_attack_vote(war_id, 2004, TEST_GUILD_ID)
        assert result["success"]
        assert result.get("is_veteran")

        war = rebellion_repo.get_war(war_id)
        # 2004 adds 1.5, so total should be inciter_weight + 1.5
        # inciter was added with 1.0 (not veteran in this setup)
        assert war["effective_attack_count"] >= 2.5

    def test_veteran_threshold_at_2_bankruptcies(self, bankruptcy_repo):
        """Exactly 2 bankruptcies = veteran."""
        from config import REBELLION_VETERAN_REBEL_MIN_BANKRUPTCIES
        assert REBELLION_VETERAN_REBEL_MIN_BANKRUPTCIES == 2


# ---------------------------------------------------------------------------
# Threshold formula
# ---------------------------------------------------------------------------


class TestThresholdFormula:
    def test_base_threshold_equal_votes(self, rebellion_service):
        """With equal attack/defend, base threshold applies."""
        threshold = rebellion_service.calculate_threshold(5.0, 5.0)
        assert threshold == 25  # REBELLION_BASE_THRESHOLD

    def test_threshold_more_defenders(self, rebellion_service):
        """More defenders lower the target, making the Wheel more likely to survive."""
        threshold = rebellion_service.calculate_threshold(5.0, 8.0)
        # net_attackers = 5 - 8 = -3, step=5 -> 25 - 15 = 10
        assert threshold == 10

    def test_threshold_more_attackers(self, rebellion_service):
        """More attackers raise the target, making the Wheel less likely to survive."""
        threshold = rebellion_service.calculate_threshold(8.0, 5.0)
        # net_attackers = 8 - 5 = 3, step=5 -> 25 + 15 = 40
        assert threshold == 40

    def test_fractional_veteran_votes_affect_threshold(self, rebellion_service):
        """Veteran half-votes should influence the battle odds, not only quorum."""
        threshold = rebellion_service.calculate_threshold(5.5, 5.0)
        assert threshold == 28

    def test_threshold_clamped_min(self, rebellion_service):
        """Threshold never goes below REBELLION_MIN_THRESHOLD."""
        from config import REBELLION_MIN_THRESHOLD
        threshold = rebellion_service.calculate_threshold(1.0, 100.0)
        assert threshold == REBELLION_MIN_THRESHOLD

    def test_threshold_clamped_max(self, rebellion_service):
        """Threshold never goes above REBELLION_MAX_THRESHOLD."""
        from config import REBELLION_MAX_THRESHOLD
        threshold = rebellion_service.calculate_threshold(100.0, 1.0)
        assert threshold == REBELLION_MAX_THRESHOLD

    def test_wheel_win_probability_uses_inclusive_threshold(self, rebellion_service):
        """A threshold of 25 means rolls 25-100 win for the Wheel: 76 outcomes."""
        assert rebellion_service.calculate_wheel_win_probability(25) == pytest.approx(0.76)
        assert rebellion_service.calculate_attacker_win_probability(25) == pytest.approx(0.24)

    def test_defenders_increase_wheel_win_probability(self, rebellion_service):
        """Extra defenders should improve the Wheel's battle odds."""
        base = rebellion_service.calculate_threshold(5.0, 5.0)
        defended = rebellion_service.calculate_threshold(5.0, 8.0)

        assert rebellion_service.calculate_wheel_win_probability(defended) > (
            rebellion_service.calculate_wheel_win_probability(base)
        )

    def test_attackers_decrease_wheel_win_probability(self, rebellion_service):
        """Extra attackers should improve the inciter/rebel battle odds."""
        base = rebellion_service.calculate_threshold(5.0, 5.0)
        attacked = rebellion_service.calculate_threshold(8.0, 5.0)

        assert rebellion_service.calculate_wheel_win_probability(attacked) < (
            rebellion_service.calculate_wheel_win_probability(base)
        )


# ---------------------------------------------------------------------------
# Quorum check
# ---------------------------------------------------------------------------


class TestQuorumCheck:
    def test_quorum_met_attack_wins(self):
        """5+ attack AND attack > defend = war declared."""
        from config import REBELLION_ATTACK_QUORUM
        eff_atk, eff_def = 5.0, 3.0
        quorum_met = eff_atk >= REBELLION_ATTACK_QUORUM
        attack_wins = eff_atk > eff_def
        assert quorum_met and attack_wins

    def test_quorum_not_met_too_few_attackers(self):
        """Only 4 effective attack = fizzle."""
        from config import REBELLION_ATTACK_QUORUM
        eff_atk = 4.5
        assert eff_atk < REBELLION_ATTACK_QUORUM

    def test_quorum_met_but_defenders_win_vote(self):
        """5+ attack but equal defend = fizzle."""
        from config import REBELLION_ATTACK_QUORUM
        eff_atk, eff_def = 5.0, 5.0
        quorum_met = eff_atk >= REBELLION_ATTACK_QUORUM
        attack_wins = eff_atk > eff_def
        assert quorum_met and not attack_wins

    def test_veteran_4_point_5_is_below_quorum(self):
        """3 vets (1.5 each) = 4.5, still below quorum of 5."""
        from config import REBELLION_ATTACK_QUORUM, REBELLION_VETERAN_REBEL_VOTE_WEIGHT
        eff_atk = 3 * REBELLION_VETERAN_REBEL_VOTE_WEIGHT
        assert eff_atk < REBELLION_ATTACK_QUORUM

    def test_veteran_4_gives_quorum(self):
        """4 vets (1.5 each) = 6.0, meets quorum."""
        from config import REBELLION_ATTACK_QUORUM, REBELLION_VETERAN_REBEL_VOTE_WEIGHT
        eff_atk = 4 * REBELLION_VETERAN_REBEL_VOTE_WEIGHT
        assert eff_atk >= REBELLION_ATTACK_QUORUM


# ---------------------------------------------------------------------------
# Full war flow (integration)
# ---------------------------------------------------------------------------


class TestWarFlow:
    def _setup_war_with_votes(self, rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
                               inciter_id=3001, n_attackers=5, n_defenders=2,
                               guild_id=TEST_GUILD_ID):
        """Helper to create a war with enough votes to trigger war declaration."""
        now = int(time.time())
        _add_player(player_repo, inciter_id, balance=100, guild_id=guild_id)
        _set_bankrupt(bankruptcy_repo, inciter_id, penalty_games=3)

        war_id = rebellion_repo.create_war(guild_id, inciter_id, now + 900, now)

        # Add additional attacker votes (inciter already counts as 1)
        for i in range(n_attackers - 1):
            aid = 3100 + i
            _add_player(player_repo, aid, balance=100, guild_id=guild_id)
            bankruptcy_repo.upsert_state(
                discord_id=aid, guild_id=guild_id,
                last_bankruptcy_at=0, penalty_games_remaining=0,
            )
            rebellion_service.process_attack_vote(war_id, aid, guild_id)

        # Add defender votes
        for j in range(n_defenders):
            did = 3200 + j
            _add_player(player_repo, did, balance=100, guild_id=guild_id)
            rebellion_service.process_defend_vote(war_id, did, guild_id)

        return war_id

    def test_attack_vote_tolerates_malformed_voter_entry(self, rebellion_repo):
        """The vote-dedup read uses .get(), so a malformed attack_voter_ids
        entry (one missing 'discord_id', e.g. legacy/corrupt data) can't raise
        KeyError inside the vote transaction and a fresh vote still records."""
        now = int(time.time())
        war_id = rebellion_repo.create_war(TEST_GUILD_ID, 3501, now + 900, now)
        # Corrupt the stored voter list with an entry missing 'discord_id'.
        with rebellion_repo.connection() as conn:
            conn.cursor().execute(
                "UPDATE wheel_wars SET attack_voter_ids = ? WHERE war_id = ?",
                (json.dumps([{"bankruptcy_count": 0}]), war_id),
            )
            conn.commit()

        result = rebellion_repo.add_attack_vote(war_id, 3502, bankruptcy_count=0)
        assert result.get("duplicate") is not True
        voters = json.loads(rebellion_repo.get_war(war_id)["attack_voter_ids"])
        assert any(v.get("discord_id") == 3502 for v in voters)

    def test_fizzle_path_refunds_defenders(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        """Fizzle returns defender stakes."""
        guild_id = TEST_GUILD_ID
        now = int(time.time())
        _add_player(player_repo, 4001, balance=100, guild_id=guild_id)
        _set_bankrupt(bankruptcy_repo, 4001, penalty_games=3)

        war_id = rebellion_repo.create_war(guild_id, 4001, now + 900, now)

        # Add 2 defenders (not enough attackers for quorum)
        for did in [4100, 4101]:
            _add_player(player_repo, did, balance=50, guild_id=guild_id)
            rebellion_service.process_defend_vote(war_id, did, guild_id)

        bal_before = {
            4100: player_repo.get_balance(4100, guild_id),
            4101: player_repo.get_balance(4101, guild_id),
        }

        rebellion_service.resolve_fizzle(war_id, guild_id)

        from config import REBELLION_DEFENDER_STAKE
        for did in [4100, 4101]:
            new_bal = player_repo.get_balance(did, guild_id)
            assert new_bal == bal_before[did] + REBELLION_DEFENDER_STAKE, \
                f"Defender {did} stake not refunded"

        war = rebellion_repo.get_war(war_id)
        assert war["status"] == "fizzled"

    def test_attacker_win_rewards(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        """Attacker win gives flat reward + stake share to all attackers."""
        guild_id = TEST_GUILD_ID
        war_id = self._setup_war_with_votes(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
            inciter_id=5001, n_attackers=5, n_defenders=2, guild_id=guild_id
        )

        war = rebellion_repo.get_war(war_id)
        attack_voters = json.loads(war["attack_voter_ids"])
        defend_voters = json.loads(war["defend_voter_ids"])
        inciter_id = war["inciter_id"]

        # Get balances before resolution
        bal_before = {v["discord_id"]: player_repo.get_balance(v["discord_id"], guild_id) for v in attack_voters}
        inciter_bal_before = player_repo.get_balance(inciter_id, guild_id)

        victory_threshold = 50
        # Force attacker win with low roll
        result = rebellion_service.resolve_battle(war_id, guild_id, battle_roll=10, victory_threshold=victory_threshold)

        assert result["outcome"] == "attackers_win"

        from config import (
            REBELLION_ATTACKER_FLAT_REWARD,
            REBELLION_DEFENDER_STAKE,
            REBELLION_INCITER_FLAT_REWARD,
        )
        # Non-inciter attackers get flat + stake share
        n_defenders = len(defend_voters)
        stake_pool = n_defenders * REBELLION_DEFENDER_STAKE
        stake_share = stake_pool // (len(attack_voters) - 1)  # inciter excluded from the pool share

        # The inciter is paid exactly the flat reward plus any pool remainder
        # that didn't divide evenly among the non-inciter attackers (the service
        # folds the remainder into the inciter so the full pool is conserved).
        # With this setup (pool=20, 4 non-inciter attackers) the remainder is 0,
        # so the inciter gets exactly the flat reward — assert it exactly so an
        # over-credit (e.g. a stray per-attacker share) is caught.
        stake_remainder = stake_pool - stake_share * (len(attack_voters) - 1)
        inciter_bal_after = player_repo.get_balance(inciter_id, guild_id)
        assert inciter_bal_after == (
            inciter_bal_before + REBELLION_INCITER_FLAT_REWARD + stake_remainder
        )

        for voter in attack_voters:
            if voter["discord_id"] == inciter_id:
                continue
            bal_after = player_repo.get_balance(voter["discord_id"], guild_id)
            expected_gain = REBELLION_ATTACKER_FLAT_REWARD + stake_share
            assert bal_after == bal_before[voter["discord_id"]] + expected_gain

    def test_attacker_win_inciter_not_double_paid(
        self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo
    ):
        """Inciter receives exactly REBELLION_INCITER_FLAT_REWARD on attacker win.

        The inciter is added to attack_voter_ids at war creation, so
        attacker_ids contains the inciter. The repo must exclude them from the
        per-attacker credit loop to avoid double-paying (flat reward + per-attacker credit).
        """
        from config import (
            REBELLION_ATTACKER_FLAT_REWARD,
            REBELLION_DEFENDER_STAKE,
            REBELLION_INCITER_FLAT_REWARD,
        )
        guild_id = TEST_GUILD_ID
        inciter_id = 5901
        war_id = self._setup_war_with_votes(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
            inciter_id=inciter_id, n_attackers=3, n_defenders=2, guild_id=guild_id
        )
        war = rebellion_repo.get_war(war_id)
        attack_voters = json.loads(war["attack_voter_ids"])
        # Verify inciter is present in attack_voter_ids (precondition of the bug)
        assert any(v["discord_id"] == inciter_id for v in attack_voters), (
            "Inciter must be in attack_voter_ids for this test to be meaningful"
        )

        inciter_bal_before = player_repo.get_balance(inciter_id, guild_id)
        result = rebellion_service.resolve_battle(
            war_id, guild_id, battle_roll=1, victory_threshold=50
        )
        assert result["outcome"] == "attackers_win"
        inciter_bal_after = player_repo.get_balance(inciter_id, guild_id)

        n_defenders = 2
        n_attackers = len(attack_voters)
        stake_pool = n_defenders * REBELLION_DEFENDER_STAKE
        stake_share = stake_pool // (n_attackers - 1)

        # Inciter gets only their flat reward — NOT flat + per_attacker_credit.
        # If double-paid, inciter_bal_after would equal
        # inciter_bal_before + REBELLION_INCITER_FLAT_REWARD
        #                      + REBELLION_ATTACKER_FLAT_REWARD + stake_share.
        assert inciter_bal_after == inciter_bal_before + REBELLION_INCITER_FLAT_REWARD, (
            f"Inciter gained {inciter_bal_after - inciter_bal_before} but expected "
            f"exactly {REBELLION_INCITER_FLAT_REWARD} (flat only). "
            f"Double-pay would be {REBELLION_INCITER_FLAT_REWARD + REBELLION_ATTACKER_FLAT_REWARD + stake_share}."
        )

    def test_attacker_win_distributes_full_defender_pool(
        self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo
    ):
        """The full defender stake pool is distributed among the non-inciter
        attackers (no coins orphaned). With N attackers incl. the inciter, the
        pool is split among the N-1 non-inciter recipients — not divided by N
        and paid to N-1 (which would leave one share undistributed).
        """
        from config import REBELLION_ATTACKER_FLAT_REWARD, REBELLION_DEFENDER_STAKE
        guild_id = TEST_GUILD_ID
        inciter_id = 5950
        # 3 attackers (incl. inciter) + 2 defenders → pool 20, split among 2 → 10 each.
        war_id = self._setup_war_with_votes(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
            inciter_id=inciter_id, n_attackers=3, n_defenders=2, guild_id=guild_id,
        )
        war = rebellion_repo.get_war(war_id)
        attack_voters = json.loads(war["attack_voter_ids"])
        defend_voters = json.loads(war["defend_voter_ids"])
        non_inciter = [
            v["discord_id"] for v in attack_voters if v["discord_id"] != inciter_id
        ]
        bal_before = {did: player_repo.get_balance(did, guild_id) for did in non_inciter}

        result = rebellion_service.resolve_battle(
            war_id, guild_id, battle_roll=1, victory_threshold=50
        )
        assert result["outcome"] == "attackers_win"

        defender_stake_pool = len(defend_voters) * REBELLION_DEFENDER_STAKE
        # Stake portion each non-inciter attacker received (gain minus flat reward).
        distributed = sum(
            (player_repo.get_balance(did, guild_id) - bal_before[did])
            - REBELLION_ATTACKER_FLAT_REWARD
            for did in non_inciter
        )
        assert distributed == defender_stake_pool, (
            f"Distributed {distributed} of the {defender_stake_pool} defender pool; "
            f"{defender_stake_pool - distributed} coins were orphaned."
        )

    def test_attacker_win_solo_inciter_receives_defender_pool(
        self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo
    ):
        """When the inciter is the sole attacker there are no non-inciter
        recipients for the defender stake pool, so the full pool folds into
        the inciter's reward instead of being destroyed (defenders paid real
        JC into the pool at vote time).
        """
        from config import REBELLION_DEFENDER_STAKE, REBELLION_INCITER_FLAT_REWARD
        guild_id = TEST_GUILD_ID
        inciter_id = 5980
        war_id = self._setup_war_with_votes(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
            inciter_id=inciter_id, n_attackers=1, n_defenders=2, guild_id=guild_id,
        )
        war = rebellion_repo.get_war(war_id)
        attack_voters = json.loads(war["attack_voter_ids"])
        defend_voters = json.loads(war["defend_voter_ids"])
        assert [v["discord_id"] for v in attack_voters] == [inciter_id], (
            "Precondition: inciter must be the sole attacker"
        )

        # Balances captured post-vote (defender stakes already debited).
        participants = [inciter_id] + list(defend_voters)
        bal_before = {did: player_repo.get_balance(did, guild_id) for did in participants}

        result = rebellion_service.resolve_battle(
            war_id, guild_id, battle_roll=1, victory_threshold=50
        )
        assert result["outcome"] == "attackers_win"

        pool = len(defend_voters) * REBELLION_DEFENDER_STAKE
        expected_reward = REBELLION_INCITER_FLAT_REWARD + pool
        assert result["inciter_reward"] == expected_reward
        inciter_gain = player_repo.get_balance(inciter_id, guild_id) - bal_before[inciter_id]
        assert inciter_gain == expected_reward, (
            f"Solo inciter gained {inciter_gain}, expected flat reward "
            f"{REBELLION_INCITER_FLAT_REWARD} + full defender pool {pool}"
        )

        # Conservation: the only minted coins are the flat reward; the staked
        # pool is fully re-credited (to the inciter), not destroyed.
        total_delta = sum(
            player_repo.get_balance(did, guild_id) - bal_before[did]
            for did in participants
        )
        assert total_delta == REBELLION_INCITER_FLAT_REWARD + pool

    def test_attacker_win_division_remainder_goes_to_inciter(
        self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo
    ):
        """When the defender pool does not divide evenly among the non-inciter
        attackers, the integer-division remainder folds into the inciter's
        reward instead of being destroyed.
        """
        from config import (
            REBELLION_ATTACKER_FLAT_REWARD,
            REBELLION_DEFENDER_STAKE,
            REBELLION_INCITER_FLAT_REWARD,
        )
        guild_id = TEST_GUILD_ID
        inciter_id = 5990
        # 5 attackers (incl. inciter) + 3 defenders → pool 30 split among 4
        # recipients → 7 each, remainder 2 to the inciter.
        war_id = self._setup_war_with_votes(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
            inciter_id=inciter_id, n_attackers=5, n_defenders=3, guild_id=guild_id,
        )
        war = rebellion_repo.get_war(war_id)
        attack_voters = json.loads(war["attack_voter_ids"])
        defend_voters = json.loads(war["defend_voter_ids"])
        attacker_ids = [v["discord_id"] for v in attack_voters]
        non_inciter = [did for did in attacker_ids if did != inciter_id]
        pool = len(defend_voters) * REBELLION_DEFENDER_STAKE
        share, remainder = divmod(pool, len(non_inciter))
        assert remainder > 0, "Precondition: pool must not divide evenly"

        bal_before = {did: player_repo.get_balance(did, guild_id) for did in attacker_ids}

        result = rebellion_service.resolve_battle(
            war_id, guild_id, battle_roll=1, victory_threshold=50
        )
        assert result["outcome"] == "attackers_win"

        inciter_gain = player_repo.get_balance(inciter_id, guild_id) - bal_before[inciter_id]
        assert inciter_gain == REBELLION_INCITER_FLAT_REWARD + remainder
        for did in non_inciter:
            gain = player_repo.get_balance(did, guild_id) - bal_before[did]
            assert gain == REBELLION_ATTACKER_FLAT_REWARD + share

        # Conservation: flat rewards minted + the full staked pool re-credited.
        total_delta = sum(
            player_repo.get_balance(did, guild_id) - bal_before[did]
            for did in attacker_ids
        )
        minted = REBELLION_INCITER_FLAT_REWARD + REBELLION_ATTACKER_FLAT_REWARD * len(non_inciter)
        assert total_delta == minted + pool

    def test_attacker_win_wheel_effects(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        """Attacker win sets WAR_SCAR and BANKRUPT_WEAKEN effects."""
        guild_id = TEST_GUILD_ID
        war_id = self._setup_war_with_votes(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
            inciter_id=6001, n_attackers=5, n_defenders=2, guild_id=guild_id
        )

        from config import REBELLION_WHEEL_EFFECT_SPINS
        rebellion_service.resolve_battle(war_id, guild_id, battle_roll=5, victory_threshold=50)

        war = rebellion_repo.get_war(war_id)
        assert war["outcome"] == "attackers_win"
        assert war["wheel_effect_spins_remaining"] == REBELLION_WHEEL_EFFECT_SPINS
        assert war["war_scar_wedge_label"] is not None
        assert war["celebration_spin_expires_at"] is not None

    def test_defender_win_rewards(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        """Defender win gives stake + reward to defenders."""
        guild_id = TEST_GUILD_ID
        war_id = self._setup_war_with_votes(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
            inciter_id=7001, n_attackers=5, n_defenders=3, guild_id=guild_id
        )

        war = rebellion_repo.get_war(war_id)
        defend_voters = json.loads(war["defend_voter_ids"])

        bal_before = {did: player_repo.get_balance(did, guild_id) for did in defend_voters}

        # Force defender win with high roll
        result = rebellion_service.resolve_battle(war_id, guild_id, battle_roll=90, victory_threshold=25)
        assert result["outcome"] == "defenders_win"

        from config import (
            REBELLION_DEFENDER_STAKE,
            REBELLION_DEFENDER_WIN_REWARD,
            REBELLION_FIRST_DEFENDER_BONUS,
        )
        for i, did in enumerate(defend_voters):
            bal_after = player_repo.get_balance(did, guild_id)
            expected = bal_before[did] + REBELLION_DEFENDER_STAKE + REBELLION_DEFENDER_WIN_REWARD
            if i == 0:
                expected += REBELLION_FIRST_DEFENDER_BONUS
            assert bal_after == expected

    def test_defender_win_adds_inciter_penalty(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        """Defender win adds 1 penalty game to inciter."""
        guild_id = TEST_GUILD_ID
        inciter_id = 8001
        war_id = self._setup_war_with_votes(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
            inciter_id=inciter_id, n_attackers=5, n_defenders=2, guild_id=guild_id
        )

        initial_penalty = bankruptcy_repo.get_penalty_games(inciter_id, guild_id)
        rebellion_service.resolve_battle(war_id, guild_id, battle_roll=90, victory_threshold=25)

        new_penalty = bankruptcy_repo.get_penalty_games(inciter_id, guild_id)
        assert new_penalty == initial_penalty + 1

    def test_inciter_not_double_paid_on_attacker_win(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        """Inciter receives exactly the flat inciter reward, not double (inciter + per-attacker)."""
        guild_id = TEST_GUILD_ID
        inciter_id = 8501
        war_id = self._setup_war_with_votes(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service,
            inciter_id=inciter_id, n_attackers=3, n_defenders=2, guild_id=guild_id,
        )
        inciter_bal_before = player_repo.get_balance(inciter_id, guild_id)

        result = rebellion_service.resolve_battle(war_id, guild_id, battle_roll=5, victory_threshold=50)
        assert result["outcome"] == "attackers_win"

        from config import (
            REBELLION_ATTACKER_FLAT_REWARD,
            REBELLION_DEFENDER_STAKE,
            REBELLION_INCITER_FLAT_REWARD,
        )
        war = rebellion_repo.get_war(war_id)
        attack_voters = json.loads(war["attack_voter_ids"])
        n_attackers = len(attack_voters)
        stake_per = (2 * REBELLION_DEFENDER_STAKE) // (n_attackers - 1)
        per_attacker = REBELLION_ATTACKER_FLAT_REWARD + stake_per

        inciter_bal_after = player_repo.get_balance(inciter_id, guild_id)
        # Inciter gets flat reward only — NOT flat reward + per_attacker credit
        expected = inciter_bal_before + REBELLION_INCITER_FLAT_REWARD
        assert inciter_bal_after == expected, (
            f"Inciter balance {inciter_bal_after} != expected {expected}; "
            f"double-pay would be {inciter_bal_before + REBELLION_INCITER_FLAT_REWARD + per_attacker}"
        )


# ---------------------------------------------------------------------------
# War spin consumption
# ---------------------------------------------------------------------------


class TestWarSpinConsumption:
    def _create_resolved_war(self, rebellion_repo, player_repo, bankruptcy_repo, guild_id=TEST_GUILD_ID):
        now = int(time.time())
        _add_player(player_repo, 9001, guild_id=guild_id)
        from config import REBELLION_WHEEL_EFFECT_SPINS
        war_id = rebellion_repo.create_war(guild_id, 9001, now + 900, now)
        rebellion_repo.set_war_outcome(
            war_id=war_id,
            outcome="attackers_win",
            battle_roll=10,
            victory_threshold=25,
            wheel_effect_spins_remaining=REBELLION_WHEEL_EFFECT_SPINS,
            war_scar_wedge_label="50",
            celebration_spin_expires_at=now + 86400,
            resolved_at=now,
        )
        return war_id

    def test_consume_decrements_spins(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        guild_id = TEST_GUILD_ID
        war_id = self._create_resolved_war(rebellion_repo, player_repo, bankruptcy_repo, guild_id)

        from config import REBELLION_WHEEL_EFFECT_SPINS
        remaining = rebellion_service.consume_war_spin(war_id, guild_id, spinner_id=9001)
        assert remaining == REBELLION_WHEEL_EFFECT_SPINS - 1

    def test_consume_stops_at_zero(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        guild_id = TEST_GUILD_ID
        war_id = self._create_resolved_war(rebellion_repo, player_repo, bankruptcy_repo, guild_id)

        # Consume all spins
        from config import REBELLION_WHEEL_EFFECT_SPINS
        for _ in range(REBELLION_WHEEL_EFFECT_SPINS):
            rebellion_service.consume_war_spin(war_id, guild_id, spinner_id=9001)

        remaining = rebellion_service.consume_war_spin(war_id, guild_id, spinner_id=9001)
        assert remaining == 0

    def test_get_active_war_effect_none_when_zero_spins(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        guild_id = TEST_GUILD_ID
        war_id = self._create_resolved_war(rebellion_repo, player_repo, bankruptcy_repo, guild_id)

        from config import REBELLION_WHEEL_EFFECT_SPINS
        for _ in range(REBELLION_WHEEL_EFFECT_SPINS):
            rebellion_service.consume_war_spin(war_id, guild_id, spinner_id=9001)

        effect = rebellion_service.get_active_war_effect(guild_id)
        assert effect is None


# ---------------------------------------------------------------------------
# Celebration spin
# ---------------------------------------------------------------------------


class TestCelebrationSpin:
    def _create_attacker_win_war(self, rebellion_repo, player_repo, guild_id=TEST_GUILD_ID, inciter_id=10001):
        now = int(time.time())
        _add_player(player_repo, inciter_id, guild_id=guild_id)
        war_id = rebellion_repo.create_war(guild_id, inciter_id, now + 900, now)
        rebellion_repo.set_war_outcome(
            war_id=war_id,
            outcome="attackers_win",
            battle_roll=10,
            victory_threshold=25,
            wheel_effect_spins_remaining=10,
            war_scar_wedge_label="50",
            celebration_spin_expires_at=int(time.time()) + 86400,
            resolved_at=now,
        )
        return war_id

    def test_each_player_can_use_once(self, rebellion_service, rebellion_repo, player_repo):
        guild_id = TEST_GUILD_ID
        war_id = self._create_attacker_win_war(rebellion_repo, player_repo, guild_id)

        _add_player(player_repo, 10100, guild_id=guild_id)
        first = rebellion_service.check_and_use_celebration_spin(war_id, 10100, guild_id)
        second = rebellion_service.check_and_use_celebration_spin(war_id, 10100, guild_id)

        assert first is True
        assert second is False

    def test_different_players_each_get_one(self, rebellion_service, rebellion_repo, player_repo):
        guild_id = TEST_GUILD_ID
        war_id = self._create_attacker_win_war(rebellion_repo, player_repo, guild_id, inciter_id=10002)

        for pid in [10200, 10201, 10202]:
            _add_player(player_repo, pid, guild_id=guild_id)
            result = rebellion_service.check_and_use_celebration_spin(war_id, pid, guild_id)
            assert result is True

    def test_expired_window_no_celebration(self, rebellion_service, rebellion_repo, player_repo):
        guild_id = TEST_GUILD_ID
        now = int(time.time())
        _add_player(player_repo, 10003, guild_id=guild_id)
        war_id = rebellion_repo.create_war(guild_id, 10003, now + 900, now)
        # Set expired celebration window
        rebellion_repo.set_war_outcome(
            war_id=war_id,
            outcome="attackers_win",
            battle_roll=10,
            victory_threshold=25,
            wheel_effect_spins_remaining=10,
            war_scar_wedge_label="50",
            celebration_spin_expires_at=now - 3600,  # Expired 1 hour ago
            resolved_at=now,
        )

        _add_player(player_repo, 10300, guild_id=guild_id)
        result = rebellion_service.check_and_use_celebration_spin(war_id, 10300, guild_id)
        assert result is False


# ---------------------------------------------------------------------------
# RETRIBUTION mechanics
# ---------------------------------------------------------------------------


class TestRetribution:
    def test_is_attacker_returns_true_for_voter(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        guild_id = TEST_GUILD_ID
        now = int(time.time())
        _add_player(player_repo, 11001, guild_id=guild_id)
        _add_player(player_repo, 11002, guild_id=guild_id)
        _set_bankrupt(bankruptcy_repo, 11001, penalty_games=3)

        war_id = rebellion_repo.create_war(guild_id, 11001, now + 900, now)
        # 11001 is the inciter (auto attack voter)
        assert rebellion_service.is_attacker(war_id, 11001) is True

    def test_is_attacker_returns_false_for_non_voter(self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo):
        guild_id = TEST_GUILD_ID
        now = int(time.time())
        _add_player(player_repo, 11003, guild_id=guild_id)
        _set_bankrupt(bankruptcy_repo, 11003, penalty_games=3)

        war_id = rebellion_repo.create_war(guild_id, 11003, now + 900, now)
        # 11004 never voted
        assert rebellion_service.is_attacker(war_id, 11004) is False


# ---------------------------------------------------------------------------
# Meta-bet parimutuel payout
# ---------------------------------------------------------------------------


class TestMetaBetPayouts:
    def _create_war_with_bets(self, rebellion_repo, player_repo, guild_id=TEST_GUILD_ID):
        now = int(time.time())
        _add_player(player_repo, 12001, balance=200, guild_id=guild_id)
        war_id = rebellion_repo.create_war(guild_id, 12001, now + 900, now)

        # Place bets: 3 on rebels (10 each), 2 on wheel (20 each)
        for pid, side, amount in [
            (12100, "rebels", 10),
            (12101, "rebels", 10),
            (12102, "rebels", 10),
            (12200, "wheel", 20),
            (12201, "wheel", 20),
        ]:
            _add_player(player_repo, pid, balance=100, guild_id=guild_id)
            rebellion_repo.place_meta_bet_atomic(war_id, guild_id, pid, side, amount, now)

        return war_id

    def test_rebels_win_payout(self, rebellion_repo, player_repo):
        guild_id = TEST_GUILD_ID
        war_id = self._create_war_with_bets(rebellion_repo, player_repo, guild_id)

        before = {pid: player_repo.get_balance(pid, guild_id) for pid in [12100, 12101, 12102, 12200, 12201]}
        result = rebellion_repo.settle_meta_bets(war_id, "rebels")

        # Total pool = 30 + 40 = 70. Each rebel bet 10 of 30, so a share floors
        # to int(70 * 10/30) = 23 (sum 69); the 1-coin remainder folds into one
        # winner so the whole 70 is paid out and nothing is destroyed.
        assert result["total_pool"] == 70
        assert result["winning_side"] == "rebels"
        payout_amounts = sorted(p["payout"] for p in result["payouts"])
        assert payout_amounts == [23, 23, 24]
        assert sum(payout_amounts) == 70, "full pool conserved (no coins burned)"
        # Each winner's balance rose by exactly their reported payout.
        for p in result["payouts"]:
            pid = p["discord_id"]
            assert player_repo.get_balance(pid, guild_id) == before[pid] + p["payout"]

        # Losers get nothing
        for pid in [12200, 12201]:
            new_bal = player_repo.get_balance(pid, guild_id)
            assert new_bal == before[pid], f"Wheel bettor {pid} should not receive payout"

    def test_wheel_win_payout(self, rebellion_repo, player_repo):
        guild_id = TEST_GUILD_ID
        war_id = self._create_war_with_bets(rebellion_repo, player_repo, guild_id)

        before = {pid: player_repo.get_balance(pid, guild_id) for pid in [12100, 12101, 12102, 12200, 12201]}
        result = rebellion_repo.settle_meta_bets(war_id, "wheel")

        # Wheel winners: 2 bettors each bet 20 of 40, split 70 evenly →
        # int(70 * 20/40) = 35 each, no remainder.
        assert result["total_pool"] == 70
        payout_amounts = sorted(p["payout"] for p in result["payouts"])
        assert payout_amounts == [35, 35]
        assert sum(payout_amounts) == 70
        for p in result["payouts"]:
            pid = p["discord_id"]
            assert player_repo.get_balance(pid, guild_id) == before[pid] + p["payout"]

        for pid in [12100, 12101, 12102]:
            new_bal = player_repo.get_balance(pid, guild_id)
            assert new_bal == before[pid], f"Rebel bettor {pid} should not receive payout"

    def test_meta_bet_remainder_folds_into_largest_stake(self, rebellion_repo, player_repo):
        """Uneven stakes produce a floor remainder; it must land on the largest
        stake and the full pool must be conserved (no coins destroyed)."""
        guild_id = TEST_GUILD_ID
        now = int(time.time())
        _add_player(player_repo, 14001, balance=200, guild_id=guild_id)
        war_id = rebellion_repo.create_war(guild_id, 14001, now + 900, now)
        # rebels pool 7 + 3 = 10; wheel pool 4; total = 14.
        # winner 14100 (7): int(14 * 7/10) = 9 ; winner 14101 (3): int(14 * 3/10) = 4
        # sum 13, remainder 1 -> folds into the larger stake (14100) -> 10.
        for pid, side, amount in [
            (14100, "rebels", 7),
            (14101, "rebels", 3),
            (14200, "wheel", 4),
        ]:
            _add_player(player_repo, pid, balance=100, guild_id=guild_id)
            rebellion_repo.place_meta_bet_atomic(war_id, guild_id, pid, side, amount, now)

        result = rebellion_repo.settle_meta_bets(war_id, "rebels")
        assert result["total_pool"] == 14
        payouts = {p["discord_id"]: p["payout"] for p in result["payouts"]}
        assert sum(payouts.values()) == 14, "nothing destroyed"
        assert payouts[14100] == 10  # 9 + 1 remainder (largest stake)
        assert payouts[14101] == 4

    def test_meta_bets_double_settle_is_noop(self, rebellion_repo, player_repo):
        """A second settle_meta_bets call must not re-pay winners."""
        guild_id = TEST_GUILD_ID
        war_id = self._create_war_with_bets(rebellion_repo, player_repo, guild_id)
        first = rebellion_repo.settle_meta_bets(war_id, "rebels")
        assert first["payouts"]
        after_first = {
            pid: player_repo.get_balance(pid, guild_id)
            for pid in [12100, 12101, 12102, 12200, 12201]
        }

        second = rebellion_repo.settle_meta_bets(war_id, "rebels")
        assert second.get("already_settled") is True
        assert second["payouts"] == []
        for pid, bal in after_first.items():
            assert player_repo.get_balance(pid, guild_id) == bal

    def test_empty_meta_bets_no_crash(self, rebellion_repo, player_repo):
        guild_id = TEST_GUILD_ID
        now = int(time.time())
        _add_player(player_repo, 13001, guild_id=guild_id)
        war_id = rebellion_repo.create_war(guild_id, 13001, now + 900, now)

        result = rebellion_repo.settle_meta_bets(war_id, "rebels")
        assert result["total_pool"] == 0
        assert result["payouts"] == []


# ---------------------------------------------------------------------------
# Startup recovery for abandoned wars (bot crash/restart mid-window)
# ---------------------------------------------------------------------------


class TestStaleWarRecovery:
    def test_recover_refunds_defenders_and_unblocks_incite(
        self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo
    ):
        """A war left in 'war' status (crash mid-battle-window) with debited
        defender stakes must be recovered on the next recover_stale_wars call:
        stakes refunded, status flipped to 'fizzled', and /incite unblocked.

        Old behavior (no recovery sweep) leaves the war active forever — the
        stakes burned and get_active_war blocking every future /incite — so
        this test fails without the fix.
        """
        from config import REBELLION_DEFENDER_STAKE

        guild_id = TEST_GUILD_ID
        now = int(time.time())
        inciter_id = 15001
        _add_player(player_repo, inciter_id, balance=100, guild_id=guild_id)
        _set_bankrupt(bankruptcy_repo, inciter_id, penalty_games=3)
        war_id = rebellion_repo.create_war(guild_id, inciter_id, now + 900, now)

        # Two defenders vote (stake debited atomically at vote time).
        defenders = [15100, 15101]
        for did in defenders:
            _add_player(player_repo, did, balance=50, guild_id=guild_id)
            rebellion_service.process_defend_vote(war_id, did, guild_id)
        bal_after_vote = {did: player_repo.get_balance(did, guild_id) for did in defenders}

        # Simulate the bot dying after the war was declared: status stuck at 'war'.
        rebellion_repo.update_war_status(war_id, "war")
        assert rebellion_repo.get_active_war(guild_id) is not None  # /incite is blocked

        recovered = rebellion_service.recover_stale_wars(guild_id)
        assert [r["war_id"] for r in recovered] == [war_id]

        # Stakes refunded.
        for did in defenders:
            assert (
                player_repo.get_balance(did, guild_id)
                == bal_after_vote[did] + REBELLION_DEFENDER_STAKE
            ), f"Defender {did} stake not refunded on recovery"

        # War terminal and /incite unblocked.
        war = rebellion_repo.get_war(war_id)
        assert war["status"] == "fizzled"
        assert rebellion_repo.get_active_war(guild_id) is None

    def test_recover_refunds_unsettled_meta_bets(
        self, rebellion_service, rebellion_repo, player_repo
    ):
        """Meta-bet stakes (debited at placement) on an abandoned war must be
        refunded by recovery; without the sweep they are lost forever."""
        guild_id = TEST_GUILD_ID
        now = int(time.time())
        _add_player(player_repo, 15201, balance=200, guild_id=guild_id)
        war_id = rebellion_repo.create_war(guild_id, 15201, now + 900, now)
        rebellion_repo.set_meta_bet_window(war_id, now + 600)  # status -> 'betting'

        bettors = {15300: ("rebels", 25), 15301: ("wheel", 40)}
        for pid, (side, amount) in bettors.items():
            _add_player(player_repo, pid, balance=100, guild_id=guild_id)
            rebellion_repo.place_meta_bet_atomic(war_id, guild_id, pid, side, amount, now)
        bal_after_bet = {pid: player_repo.get_balance(pid, guild_id) for pid in bettors}

        recovered = rebellion_service.recover_stale_wars(guild_id)
        assert recovered and recovered[0]["meta_bets_refunded"] == 2

        for pid, (_side, amount) in bettors.items():
            assert (
                player_repo.get_balance(pid, guild_id) == bal_after_bet[pid] + amount
            ), f"Meta-bettor {pid} stake not refunded on recovery"
        assert rebellion_repo.get_war(war_id)["status"] == "fizzled"

    def test_recover_is_idempotent(
        self, rebellion_service, rebellion_repo, player_repo
    ):
        """A second recover_stale_wars call must not double-refund anything."""
        guild_id = TEST_GUILD_ID
        now = int(time.time())
        _add_player(player_repo, 15401, balance=200, guild_id=guild_id)
        war_id = rebellion_repo.create_war(guild_id, 15401, now + 900, now)
        _add_player(player_repo, 15402, balance=100, guild_id=guild_id)
        rebellion_repo.place_meta_bet_atomic(war_id, guild_id, 15402, "rebels", 30, now)

        first = rebellion_service.recover_stale_wars(guild_id)
        assert first  # recovered once
        bal_after_first = player_repo.get_balance(15402, guild_id)

        second = rebellion_service.recover_stale_wars(guild_id)
        assert second == [], "war already terminal; nothing to recover"
        assert player_repo.get_balance(15402, guild_id) == bal_after_first

    def test_recover_all_guilds_when_guild_id_none(
        self, rebellion_service, rebellion_repo, player_repo
    ):
        """The startup sweep passes guild_id=None to recover across all guilds."""
        now = int(time.time())
        guild_a, guild_b = TEST_GUILD_ID, TEST_GUILD_ID + 777
        wars = []
        for g in (guild_a, guild_b):
            inciter = 15500 + g % 1000
            _add_player(player_repo, inciter, balance=100, guild_id=g)
            war_id = rebellion_repo.create_war(g, inciter, now + 900, now)
            rebellion_repo.update_war_status(war_id, "war")
            wars.append(war_id)

        recovered_ids = {r["war_id"] for r in rebellion_service.recover_stale_wars(None)}
        assert set(wars).issubset(recovered_ids)
        for g in (guild_a, guild_b):
            assert rebellion_repo.get_active_war(g) is None


# ---------------------------------------------------------------------------
# Battle resolution + meta-bet settlement atomicity
# ---------------------------------------------------------------------------


class TestResolutionSettlementAtomicity:
    def _war_with_meta_bets(self, rebellion_repo, player_repo, bankruptcy_repo,
                            rebellion_service, inciter_id, guild_id=TEST_GUILD_ID):
        now = int(time.time())
        _add_player(player_repo, inciter_id, balance=200, guild_id=guild_id)
        _set_bankrupt(bankruptcy_repo, inciter_id, penalty_games=3)
        war_id = rebellion_repo.create_war(guild_id, inciter_id, now + 900, now)
        # A couple attackers so quorum/threshold context is realistic.
        for i in range(4):
            aid = 16100 + i
            _add_player(player_repo, aid, balance=100, guild_id=guild_id)
            bankruptcy_repo.upsert_state(
                discord_id=aid, guild_id=guild_id,
                last_bankruptcy_at=0, penalty_games_remaining=0,
            )
            rebellion_service.process_attack_vote(war_id, aid, guild_id)
        # Meta-bets on both sides.
        for pid, side, amount in [
            (16200, "rebels", 30),
            (16201, "wheel", 20),
        ]:
            _add_player(player_repo, pid, balance=100, guild_id=guild_id)
            rebellion_repo.place_meta_bet_atomic(war_id, guild_id, pid, side, amount, now)
        return war_id

    def test_meta_bets_settled_inside_resolution_transaction(
        self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo
    ):
        """resolve_battle must settle the meta-bets in the SAME commit as the
        war-status flip — so when the war is 'resolved', the bets are already
        paid (no separate, loseable post-resolve settle step).

        Before the fix, resolve_battle flipped the status and a *separate*
        settle_meta_bets call did the payout; a crash between them left a
        'resolved' war with debited-but-unpaid stakes. This asserts the result
        carries the settle summary AND winners are already credited once the
        war is resolved.
        """
        guild_id = TEST_GUILD_ID
        war_id = self._war_with_meta_bets(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service, inciter_id=16001
        )
        rebel_bettor, wheel_bettor = 16200, 16201
        bal_before = {
            rebel_bettor: player_repo.get_balance(rebel_bettor, guild_id),
            wheel_bettor: player_repo.get_balance(wheel_bettor, guild_id),
        }

        # Force attacker (rebels) win — low roll under the threshold.
        result = rebellion_service.resolve_battle(
            war_id, guild_id, battle_roll=1, victory_threshold=50
        )
        assert result["outcome"] == "attackers_win"

        # War is resolved AND the settle summary rode along in the result.
        assert rebellion_repo.get_war(war_id)["status"] == "resolved"
        meta = result["meta_bet_result"]
        assert meta["winning_side"] == "rebels"
        assert meta["total_pool"] == 50  # 30 + 20

        # Winner already paid the full pool; loser unchanged — all committed
        # by the time the war reads 'resolved'.
        assert player_repo.get_balance(rebel_bettor, guild_id) == bal_before[rebel_bettor] + 50
        assert player_repo.get_balance(wheel_bettor, guild_id) == bal_before[wheel_bettor]

        # Bets are persisted as settled (payout non-NULL), so a recovery sweep
        # or a stray settle is a no-op — settlement can't be lost or repeated.
        bets = rebellion_repo.get_meta_bets(war_id)
        assert all(b["payout"] is not None for b in bets)
        assert rebellion_repo.settle_meta_bets(war_id, "rebels").get("already_settled") is True

    def test_defenders_win_settles_meta_bets_atomically(
        self, rebellion_service, rebellion_repo, player_repo, bankruptcy_repo
    ):
        """The defenders-win path likewise settles meta-bets (winning_side
        'wheel') inside the resolution transaction."""
        guild_id = TEST_GUILD_ID
        war_id = self._war_with_meta_bets(
            rebellion_repo, player_repo, bankruptcy_repo, rebellion_service, inciter_id=16002
        )
        wheel_bettor = 16201
        bal_before = player_repo.get_balance(wheel_bettor, guild_id)

        # Force defenders (wheel) win — high roll at/above threshold.
        result = rebellion_service.resolve_battle(
            war_id, guild_id, battle_roll=99, victory_threshold=25
        )
        assert result["outcome"] == "defenders_win"
        assert rebellion_repo.get_war(war_id)["status"] == "resolved"
        meta = result["meta_bet_result"]
        assert meta["winning_side"] == "wheel"
        assert meta["total_pool"] == 50
        # Sole wheel bettor takes the whole pool.
        assert player_repo.get_balance(wheel_bettor, guild_id) == bal_before + 50


# ---------------------------------------------------------------------------
# apply_war_effects (wheel drawing utility)
# ---------------------------------------------------------------------------


class TestApplyWarEffects:
    def _get_normal_wedges(self):
        from utils.wheel_drawing import get_wheel_wedges
        return get_wheel_wedges(is_bankrupt=False, is_golden=False)

    def test_attacker_win_scars_first_matching_wedge(self):
        from utils.wheel_drawing import apply_war_effects
        war_state = {"outcome": "attackers_win", "war_scar_wedge_label": "50"}
        wedges = self._get_normal_wedges()
        modified = apply_war_effects(wedges, war_state)

        scar_wedges = [w for w in modified if w[0] == "WAR SCAR 💀"]
        assert len(scar_wedges) >= 1
        assert scar_wedges[0][1] == 0

    def test_attacker_win_weakens_bankrupt(self):
        from config import REBELLION_BANKRUPT_WEAKEN_RATE
        from utils.wheel_drawing import apply_war_effects
        war_state = {"outcome": "attackers_win", "war_scar_wedge_label": "50"}
        wedges = self._get_normal_wedges()

        # Find BANKRUPT value before
        bankrupt_before = next((value for label, value, _color in wedges if label == "BANKRUPT"), None)
        modified = apply_war_effects(wedges, war_state)
        bankrupt_after = next((value for label, value, _color in modified if label == "BANKRUPT"), None)

        if bankrupt_before is not None and isinstance(bankrupt_before, int):
            expected = max(-1, int(bankrupt_before * (1.0 - REBELLION_BANKRUPT_WEAKEN_RATE)))
            assert bankrupt_after == expected

    def test_defender_win_adds_trophy_and_retribution(self):
        from utils.wheel_drawing import apply_war_effects
        war_state = {"outcome": "defenders_win"}
        wedges = self._get_normal_wedges()
        modified = apply_war_effects(wedges, war_state)

        labels = [w[0] for w in modified]
        assert "WAR TROPHY 🏆" in labels
        assert "RETRIBUTION ⚔️" in labels

    def test_defender_win_strengthens_bankrupt(self):
        from config import REBELLION_BANKRUPT_STRENGTHEN_RATE
        from utils.wheel_drawing import apply_war_effects
        war_state = {"outcome": "defenders_win"}
        wedges = self._get_normal_wedges()

        bankrupt_before = next((value for label, value, _color in wedges if label == "BANKRUPT"), None)
        modified = apply_war_effects(wedges, war_state)
        bankrupt_after = next((value for label, value, _color in modified if label == "BANKRUPT"), None)

        if bankrupt_before is not None and isinstance(bankrupt_before, int):
            expected = int(bankrupt_before * (1.0 + REBELLION_BANKRUPT_STRENGTHEN_RATE))
            assert bankrupt_after == expected

    def test_no_war_state_returns_unchanged(self):
        from utils.wheel_drawing import apply_war_effects
        war_state = {"outcome": None}
        wedges = self._get_normal_wedges()
        modified = apply_war_effects(wedges, war_state)
        assert modified == wedges


# ---------------------------------------------------------------------------
# War stats: exact-membership counting
# ---------------------------------------------------------------------------


class TestWarStats:
    def _finalize_war(self, rebellion_repo, war_id, outcome, defend_voters):
        """Set outcome and defend voter JSON directly for a war."""
        with rebellion_repo.connection() as conn:
            conn.execute(
                "UPDATE wheel_wars SET outcome = ?, defend_voter_ids = ? WHERE war_id = ?",
                (outcome, json.dumps(defend_voters), war_id),
            )

    def test_substring_ids_not_cross_counted(self, rebellion_repo):
        """get_player_war_stats must count exact voter membership, not substrings.

        discord_id 123 must not be credited with wars that only contain
        1234 / 51234 in the voter JSON arrays.
        """
        guild_id = TEST_GUILD_ID
        now = int(time.time())

        # War A: attacked & defended only by 1234 (a superstring of 123).
        war_a = rebellion_repo.create_war(guild_id, 999001, now + 900, now)
        rebellion_repo.add_attack_vote(war_a, 1234, bankruptcy_count=0)
        self._finalize_war(rebellion_repo, war_a, "attackers_win", [1234])

        # War B: attacked & defended by 123 itself.
        war_b = rebellion_repo.create_war(guild_id, 999002, now + 900, now)
        rebellion_repo.add_attack_vote(war_b, 123, bankruptcy_count=0)
        self._finalize_war(rebellion_repo, war_b, "defenders_win", [123])

        stats_123 = rebellion_repo.get_player_war_stats(123, guild_id)
        # 123 only attacked/defended war B, not war A.
        assert sum(stats_123["attacked"].values()) == 1
        assert sum(stats_123["defended"].values()) == 1
        assert stats_123["attacked"].get("attackers_win", 0) == 0
        assert stats_123["defended"].get("attackers_win", 0) == 0

        stats_1234 = rebellion_repo.get_player_war_stats(1234, guild_id)
        # 1234 only attacked/defended war A, not war B.
        assert sum(stats_1234["attacked"].values()) == 1
        assert sum(stats_1234["defended"].values()) == 1
        assert stats_1234["attacked"].get("defenders_win", 0) == 0
        assert stats_1234["defended"].get("defenders_win", 0) == 0
