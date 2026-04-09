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
        """More defenders = higher threshold (wheel harder to beat)."""
        threshold = rebellion_service.calculate_threshold(5.0, 8.0)
        # net_defenders = 8 - 5 = 3, step=5 → 25 + 15 = 40
        assert threshold == 40

    def test_threshold_more_attackers(self, rebellion_service):
        """More attackers = lower threshold (wheel easier to beat)."""
        threshold = rebellion_service.calculate_threshold(8.0, 5.0)
        # net_defenders = 5 - 8 = -3, step=5 → 25 - 15 = 10
        assert threshold == 10

    def test_threshold_clamped_min(self, rebellion_service):
        """Threshold never goes below REBELLION_MIN_THRESHOLD."""
        from config import REBELLION_MIN_THRESHOLD
        threshold = rebellion_service.calculate_threshold(100.0, 1.0)
        assert threshold == REBELLION_MIN_THRESHOLD

    def test_threshold_clamped_max(self, rebellion_service):
        """Threshold never goes above REBELLION_MAX_THRESHOLD."""
        from config import REBELLION_MAX_THRESHOLD
        threshold = rebellion_service.calculate_threshold(1.0, 100.0)
        assert threshold == REBELLION_MAX_THRESHOLD


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
        inciter_bal_after = player_repo.get_balance(inciter_id, guild_id)
        assert inciter_bal_after >= inciter_bal_before + REBELLION_INCITER_FLAT_REWARD

        # Non-inciter attackers get flat + stake share
        n_defenders = len(defend_voters)
        stake_pool = n_defenders * REBELLION_DEFENDER_STAKE
        stake_share = stake_pool // len(attack_voters)

        for voter in attack_voters:
            if voter["discord_id"] == inciter_id:
                continue
            bal_after = player_repo.get_balance(voter["discord_id"], guild_id)
            expected_gain = REBELLION_ATTACKER_FLAT_REWARD + stake_share
            assert bal_after == bal_before[voter["discord_id"]] + expected_gain

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
            rebellion_repo.place_meta_bet_atomic(war_id, guild_id, pid, side, amount, now, max_debt=500)

        return war_id

    def test_rebels_win_payout(self, rebellion_repo, player_repo):
        guild_id = TEST_GUILD_ID
        war_id = self._create_war_with_bets(rebellion_repo, player_repo, guild_id)

        before = {pid: player_repo.get_balance(pid, guild_id) for pid in [12100, 12101, 12102, 12200, 12201]}
        result = rebellion_repo.settle_meta_bets(war_id, "rebels")

        # Total pool = 30 + 40 = 70
        # Rebel winners split 70 proportionally: each bet 10 out of 30, so each gets 70 * (10/30) = 23
        assert result["total_pool"] == 70
        assert result["winning_side"] == "rebels"
        for pid in [12100, 12101, 12102]:
            new_bal = player_repo.get_balance(pid, guild_id)
            assert new_bal > before[pid], f"Rebel bettor {pid} should profit"

        # Losers get nothing
        for pid in [12200, 12201]:
            new_bal = player_repo.get_balance(pid, guild_id)
            assert new_bal == before[pid], f"Wheel bettor {pid} should not receive payout"

    def test_wheel_win_payout(self, rebellion_repo, player_repo):
        guild_id = TEST_GUILD_ID
        war_id = self._create_war_with_bets(rebellion_repo, player_repo, guild_id)

        before = {pid: player_repo.get_balance(pid, guild_id) for pid in [12100, 12101, 12102, 12200, 12201]}
        result = rebellion_repo.settle_meta_bets(war_id, "wheel")

        # Wheel winners: 2 bettors each bet 20 out of 40, split 70
        # Each gets 70 * (20/40) = 35
        assert result["total_pool"] == 70
        for pid in [12200, 12201]:
            new_bal = player_repo.get_balance(pid, guild_id)
            assert new_bal > before[pid], f"Wheel bettor {pid} should profit"

        for pid in [12100, 12101, 12102]:
            new_bal = player_repo.get_balance(pid, guild_id)
            assert new_bal == before[pid], f"Rebel bettor {pid} should not receive payout"

    def test_empty_meta_bets_no_crash(self, rebellion_repo, player_repo):
        guild_id = TEST_GUILD_ID
        now = int(time.time())
        _add_player(player_repo, 13001, guild_id=guild_id)
        war_id = rebellion_repo.create_war(guild_id, 13001, now + 900, now)

        result = rebellion_repo.settle_meta_bets(war_id, "rebels")
        assert result["total_pool"] == 0
        assert result["payouts"] == []


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
