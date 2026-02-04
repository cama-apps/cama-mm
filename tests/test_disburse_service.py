"""
Tests for DisburseService - nonprofit fund distribution voting and distribution.
"""

import pytest
import time

from services.disburse_service import DisburseService
from services.loan_service import LoanRepository
from repositories.disburse_repository import DisburseRepository
from repositories.player_repository import PlayerRepository

TEST_GUILD_ID = 12345


@pytest.fixture
def disburse_repo(repo_db_path):
    """Create a DisburseRepository for testing."""
    return DisburseRepository(repo_db_path)


@pytest.fixture
def loan_repo(repo_db_path):
    """Create a LoanRepository for testing."""
    return LoanRepository(repo_db_path)


@pytest.fixture
def player_repo(repo_db_path):
    """Create a PlayerRepository for testing."""
    return PlayerRepository(repo_db_path)


@pytest.fixture
def disburse_service(disburse_repo, player_repo, loan_repo):
    """Create a DisburseService for testing."""
    return DisburseService(
        disburse_repo=disburse_repo,
        player_repo=player_repo,
        loan_repo=loan_repo,
        min_fund=100,  # Lower threshold for testing
        quorum_percentage=0.40,
    )


@pytest.fixture
def setup_players(player_repo):
    """Create test players with various balances."""
    # Create 5 players: 2 with negative balance, 3 with positive
    player_repo.add(discord_id=1001, discord_username="Debtor1", guild_id=TEST_GUILD_ID, initial_mmr=3000)
    player_repo.add(discord_id=1002, discord_username="Debtor2", guild_id=TEST_GUILD_ID, initial_mmr=3000)
    player_repo.add(discord_id=1003, discord_username="Voter1", guild_id=TEST_GUILD_ID, initial_mmr=3000)
    player_repo.add(discord_id=1004, discord_username="Voter2", guild_id=TEST_GUILD_ID, initial_mmr=3000)
    player_repo.add(discord_id=1005, discord_username="Voter3", guild_id=TEST_GUILD_ID, initial_mmr=3000)

    # Set balances
    player_repo.update_balance(1001, TEST_GUILD_ID, -100)  # Debtor1: -100
    player_repo.update_balance(1002, TEST_GUILD_ID, -50)   # Debtor2: -50
    player_repo.update_balance(1003, TEST_GUILD_ID, 100)   # Voter1: +100
    player_repo.update_balance(1004, TEST_GUILD_ID, 100)   # Voter2: +100
    player_repo.update_balance(1005, TEST_GUILD_ID, 100)   # Voter3: +100


@pytest.fixture
def setup_nonprofit_fund(loan_repo):
    """Add funds to the nonprofit fund."""
    loan_repo.add_to_nonprofit_fund(guild_id=TEST_GUILD_ID, amount=300)


class TestEvenDistribution:
    """Test even distribution calculation."""

    def test_even_distribution_basic(self, disburse_service):
        """Test even split between two debtors."""
        debtors = [
            {"discord_id": 1001, "balance": -100},
            {"discord_id": 1002, "balance": -100},
        ]
        distributions = disburse_service._calculate_even_distribution(200, debtors)

        assert len(distributions) == 2
        amounts = {d[0]: d[1] for d in distributions}
        assert amounts[1001] == 100
        assert amounts[1002] == 100

    def test_even_distribution_capped_at_debt(self, disburse_service):
        """Test that distribution is capped at each player's debt."""
        debtors = [
            {"discord_id": 1001, "balance": -10},  # Only needs 10
            {"discord_id": 1002, "balance": -500},  # Needs 500
        ]
        distributions = disburse_service._calculate_even_distribution(200, debtors)

        amounts = {d[0]: d[1] for d in distributions}
        # Player 1001 should only get 10 (their debt)
        # Player 1002 should get the remaining 190
        assert amounts[1001] == 10
        assert amounts[1002] == 190

    def test_even_distribution_excess_fund(self, disburse_service):
        """Test when fund exceeds total debt."""
        debtors = [
            {"discord_id": 1001, "balance": -30},
            {"discord_id": 1002, "balance": -20},
        ]
        distributions = disburse_service._calculate_even_distribution(100, debtors)

        amounts = {d[0]: d[1] for d in distributions}
        # Total debt is 50, so only 50 should be distributed
        total_distributed = sum(amounts.values())
        assert total_distributed == 50
        assert amounts[1001] == 30
        assert amounts[1002] == 20

    def test_even_distribution_many_small_debts(self, disburse_service):
        """Test even distribution with many small debts."""
        debtors = [
            {"discord_id": i, "balance": -5}
            for i in range(1, 11)  # 10 players, each -5 debt
        ]
        distributions = disburse_service._calculate_even_distribution(100, debtors)

        # Total debt is 50, so only 50 should be distributed
        # Each should get 5
        amounts = {d[0]: d[1] for d in distributions}
        assert sum(amounts.values()) == 50
        for pid, amount in amounts.items():
            assert amount == 5


class TestProportionalDistribution:
    """Test proportional distribution calculation."""

    def test_proportional_distribution_basic(self, disburse_service):
        """Test proportional split based on debt."""
        debtors = [
            {"discord_id": 1001, "balance": -300},  # 60% of total debt
            {"discord_id": 1002, "balance": -200},  # 40% of total debt
        ]
        distributions = disburse_service._calculate_proportional_distribution(100, debtors)

        amounts = {d[0]: d[1] for d in distributions}
        # Should be roughly 60/40 split
        assert amounts[1001] >= 55  # ~60
        assert amounts[1002] >= 35  # ~40
        assert sum(amounts.values()) == 100

    def test_proportional_distribution_capped(self, disburse_service):
        """Test proportional distribution capped at debt."""
        debtors = [
            {"discord_id": 1001, "balance": -10},   # Would get 50% but only needs 10
            {"discord_id": 1002, "balance": -1000},  # Gets the rest
        ]
        distributions = disburse_service._calculate_proportional_distribution(100, debtors)

        amounts = {d[0]: d[1] for d in distributions}
        assert amounts[1001] <= 10  # Capped at debt
        assert sum(amounts.values()) <= 100


class TestNeediestDistribution:
    """Test neediest distribution calculation."""

    def test_neediest_distribution_basic(self, disburse_service):
        """Test all funds go to most indebted player."""
        debtors = [
            {"discord_id": 1001, "balance": -100},
            {"discord_id": 1002, "balance": -500},  # Neediest
            {"discord_id": 1003, "balance": -50},
        ]
        distributions = disburse_service._calculate_neediest_distribution(200, debtors)

        assert len(distributions) == 1
        assert distributions[0][0] == 1002  # Neediest player
        assert distributions[0][1] == 200

    def test_neediest_distribution_capped(self, disburse_service):
        """Test neediest distribution capped at debt."""
        debtors = [
            {"discord_id": 1001, "balance": -50},  # Only needs 50
        ]
        distributions = disburse_service._calculate_neediest_distribution(200, debtors)

        assert len(distributions) == 1
        assert distributions[0][0] == 1001
        assert distributions[0][1] == 50  # Capped at debt


class TestProposalLifecycle:
    """Test proposal creation and voting lifecycle."""

    def test_can_propose_insufficient_fund(
        self, disburse_service, setup_players
    ):
        """Test proposal blocked when fund is below minimum."""
        can, reason = disburse_service.can_propose(guild_id=TEST_GUILD_ID)
        assert not can
        assert reason.startswith("insufficient_fund:")

    def test_can_propose_success(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test proposal can be created when conditions are met."""
        can, reason = disburse_service.can_propose(guild_id=TEST_GUILD_ID)
        assert can
        assert reason == ""

    def test_can_propose_no_eligible_recipients(
        self, disburse_service, player_repo, setup_nonprofit_fund
    ):
        """Test proposal blocked when no debtors and insufficient non-debtors for stimulus."""
        # Create only 1 player with positive balance (not enough for stimulus)
        player_repo.add(discord_id=9999, discord_username="RichGuy", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.update_balance(9999, TEST_GUILD_ID, 1000)

        can, reason = disburse_service.can_propose(guild_id=TEST_GUILD_ID)
        assert not can
        assert reason == "no_eligible_recipients"

    def test_create_proposal(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test proposal creation."""
        proposal = disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        assert proposal is not None
        assert proposal.fund_amount == 300
        assert proposal.status == "active"
        assert proposal.quorum_required >= 1  # At least 1 for 5 players

    def test_cannot_create_duplicate_proposal(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that duplicate proposals are blocked."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        can, reason = disburse_service.can_propose(guild_id=TEST_GUILD_ID)
        assert not can
        assert reason == "active_proposal_exists"

    def test_add_vote(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test voting on a proposal."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        result = disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="even")

        assert result["votes"]["even"] == 1
        assert result["total_votes"] == 1
        assert not result["quorum_reached"]

    def test_vote_change(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that a player can change their vote."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Vote even
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="even")
        # Change to proportional
        result = disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="proportional")

        # Should have 1 vote for proportional, 0 for even
        assert result["votes"]["proportional"] == 1
        assert result["votes"]["even"] == 0
        assert result["total_votes"] == 1


class TestQuorumAndExecution:
    """Test quorum checking and disbursement execution."""

    def test_quorum_calculation(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test quorum is correctly calculated."""
        proposal = disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # 5 players, 40% quorum = 2 votes needed
        assert proposal.quorum_required == 2

    def test_quorum_reached(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test quorum detection."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Add 2 votes (40% of 5 players)
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="even")
        result = disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1004, method="even")

        assert result["quorum_reached"]

    def test_tie_breaker_even_wins(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that ties are broken in favor of even split."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Add votes that result in a tie
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="even")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1004, method="proportional")

        quorum_reached, winner = disburse_service.check_quorum(guild_id=TEST_GUILD_ID)
        assert quorum_reached
        assert winner == "even"  # Tie breaker

    def test_execute_disbursement(
        self, disburse_service, player_repo, setup_players, setup_nonprofit_fund
    ):
        """Test full disbursement execution."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Vote for even split
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="even")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1004, method="even")

        result = disburse_service.execute_disbursement(guild_id=TEST_GUILD_ID)

        assert result["success"]
        assert result["method"] == "even"
        assert result["total_disbursed"] > 0
        assert result["recipient_count"] == 2  # Two debtors

        # Check that debtors received funds
        debtor1_balance = player_repo.get_balance(1001, TEST_GUILD_ID)
        debtor2_balance = player_repo.get_balance(1002, TEST_GUILD_ID)
        assert debtor1_balance > -100  # Was -100
        assert debtor2_balance > -50   # Was -50

    def test_disbursement_marks_complete(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that disbursement marks proposal as completed."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="even")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1004, method="even")

        disburse_service.execute_disbursement(guild_id=TEST_GUILD_ID)

        # Should be able to create a new proposal now
        can, reason = disburse_service.can_propose(guild_id=TEST_GUILD_ID)
        # Note: might fail due to no more funds, but not due to active proposal
        assert reason != "active_proposal_exists"


class TestResetProposal:
    """Test proposal reset functionality."""

    def test_reset_proposal(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test resetting an active proposal."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="even")

        success = disburse_service.reset_proposal(guild_id=TEST_GUILD_ID)
        assert success

        # Should be able to create a new proposal
        can, reason = disburse_service.can_propose(guild_id=TEST_GUILD_ID)
        assert can

    def test_reset_no_proposal(self, disburse_service):
        """Test resetting when no proposal exists."""
        success = disburse_service.reset_proposal(guild_id=TEST_GUILD_ID)
        assert not success


class TestDisbursementHistory:
    """Test disbursement history tracking."""

    def test_get_last_disbursement(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test retrieving last disbursement info."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="even")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1004, method="even")
        disburse_service.execute_disbursement(guild_id=TEST_GUILD_ID)

        last = disburse_service.get_last_disbursement(guild_id=TEST_GUILD_ID)

        assert last is not None
        assert last["method"] == "even"
        assert last["total_amount"] > 0
        assert last["recipient_count"] == 2
        assert len(last["recipients"]) == 2

    def test_no_history(self, disburse_service):
        """Test when no disbursement history exists."""
        last = disburse_service.get_last_disbursement(guild_id=TEST_GUILD_ID)
        assert last is None


class TestStimulusDistribution:
    """Test stimulus distribution calculation."""

    def test_stimulus_distribution_basic(self, disburse_service):
        """Test even split among eligible players."""
        # 4 eligible players (non-debtors, not top 3)
        eligible = [
            {"discord_id": 1001, "balance": 50},
            {"discord_id": 1002, "balance": 40},
            {"discord_id": 1003, "balance": 30},
            {"discord_id": 1004, "balance": 20},
        ]
        distributions = disburse_service._calculate_stimulus_distribution(100, eligible)

        assert len(distributions) == 4
        amounts = {d[0]: d[1] for d in distributions}
        # 100 / 4 = 25 each
        assert sum(amounts.values()) == 100
        for pid, amount in amounts.items():
            assert amount == 25

    def test_stimulus_distribution_with_remainder(self, disburse_service):
        """Test stimulus split with remainder distributed to first players."""
        eligible = [
            {"discord_id": 1001, "balance": 50},
            {"discord_id": 1002, "balance": 40},
            {"discord_id": 1003, "balance": 30},
        ]
        distributions = disburse_service._calculate_stimulus_distribution(100, eligible)

        amounts = {d[0]: d[1] for d in distributions}
        # 100 / 3 = 33 each, with 1 remainder
        assert sum(amounts.values()) == 100
        # First player gets remainder
        assert amounts[1001] == 34
        assert amounts[1002] == 33
        assert amounts[1003] == 33

    def test_stimulus_distribution_empty(self, disburse_service):
        """Test stimulus with no eligible players."""
        distributions = disburse_service._calculate_stimulus_distribution(100, [])
        assert distributions == []

    def test_stimulus_distribution_single_player(self, disburse_service):
        """Test stimulus with single eligible player."""
        eligible = [{"discord_id": 1001, "balance": 50}]
        distributions = disburse_service._calculate_stimulus_distribution(100, eligible)

        assert len(distributions) == 1
        assert distributions[0] == (1001, 100)


class TestLotteryEligibility:
    """Test lottery eligibility in repository."""

    def test_get_all_registered_players_for_lottery(self, player_repo):
        """Test that all registered players are returned for lottery."""
        # Create 5 players
        for i in range(1, 6):
            player_repo.add(discord_id=i, discord_username=f"Player{i}", guild_id=TEST_GUILD_ID, initial_mmr=3000)

        eligible = player_repo.get_all_registered_players_for_lottery(TEST_GUILD_ID)

        assert len(eligible) == 5
        eligible_ids = {p["discord_id"] for p in eligible}
        assert eligible_ids == {1, 2, 3, 4, 5}

    def test_get_all_registered_players_for_lottery_includes_debtors(self, player_repo):
        """Test that debtors are included in lottery."""
        player_repo.add(discord_id=1, discord_username="Rich", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="Debtor", guild_id=TEST_GUILD_ID, initial_mmr=3000)

        player_repo.update_balance(1, TEST_GUILD_ID, 100)
        player_repo.update_balance(2, TEST_GUILD_ID, -100)  # Debtor

        eligible = player_repo.get_all_registered_players_for_lottery(TEST_GUILD_ID)

        eligible_ids = {p["discord_id"] for p in eligible}
        assert 1 in eligible_ids
        assert 2 in eligible_ids  # Debtors included


class TestSocialSecurityEligibility:
    """Test social security eligibility in repository."""

    def test_get_players_by_games_played_basic(self, player_repo):
        """Test that players are sorted by games played."""
        for i in range(1, 4):
            player_repo.add(discord_id=i, discord_username=f"Player{i}", guild_id=TEST_GUILD_ID, initial_mmr=3000)

        # Set different game counts
        with player_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("UPDATE players SET wins = 10, losses = 5 WHERE discord_id = 1 AND guild_id = ?", (TEST_GUILD_ID,))
            cursor.execute("UPDATE players SET wins = 20, losses = 10 WHERE discord_id = 2 AND guild_id = ?", (TEST_GUILD_ID,))
            cursor.execute("UPDATE players SET wins = 5, losses = 5 WHERE discord_id = 3 AND guild_id = ?", (TEST_GUILD_ID,))

        players = player_repo.get_players_by_games_played(TEST_GUILD_ID)

        assert len(players) == 3
        # Should be sorted by games DESC
        assert players[0]["discord_id"] == 2  # 30 games
        assert players[1]["discord_id"] == 1  # 15 games
        assert players[2]["discord_id"] == 3  # 10 games
        assert players[0]["games_played"] == 30

    def test_get_players_by_games_played_excludes_zero_games(self, player_repo):
        """Test that players with 0 games are excluded."""
        player_repo.add(discord_id=1, discord_username="Veteran", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="Newbie", guild_id=TEST_GUILD_ID, initial_mmr=3000)

        with player_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("UPDATE players SET wins = 10, losses = 5 WHERE discord_id = 1 AND guild_id = ?", (TEST_GUILD_ID,))
            # Player 2 has 0 wins, 0 losses (default)

        players = player_repo.get_players_by_games_played(TEST_GUILD_ID)

        assert len(players) == 1
        assert players[0]["discord_id"] == 1


class TestStimulusEligibility:
    """Test stimulus eligibility in repository."""

    def test_get_stimulus_eligible_excludes_top_3(self, player_repo):
        """Test that top 3 by balance are excluded."""
        # Create 6 players with varying balances
        player_repo.add(discord_id=1, discord_username="Rich1", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="Rich2", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=3, discord_username="Rich3", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=4, discord_username="Mid1", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=5, discord_username="Mid2", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=6, discord_username="Poor1", guild_id=TEST_GUILD_ID, initial_mmr=3000)

        player_repo.update_balance(1, TEST_GUILD_ID, 1000)  # Top 1
        player_repo.update_balance(2, TEST_GUILD_ID, 500)   # Top 2
        player_repo.update_balance(3, TEST_GUILD_ID, 200)   # Top 3
        player_repo.update_balance(4, TEST_GUILD_ID, 50)    # Eligible
        player_repo.update_balance(5, TEST_GUILD_ID, 10)    # Eligible
        player_repo.update_balance(6, TEST_GUILD_ID, 0)     # Eligible (zero balance is non-negative)

        eligible = player_repo.get_stimulus_eligible_players(TEST_GUILD_ID)

        # Should only return players 4, 5, 6
        eligible_ids = {p["discord_id"] for p in eligible}
        assert eligible_ids == {4, 5, 6}

    def test_get_stimulus_eligible_excludes_debtors(self, player_repo):
        """Test that players with negative balance are excluded."""
        player_repo.add(discord_id=1, discord_username="Rich", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="Mid", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=3, discord_username="Zero", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=4, discord_username="Debtor", guild_id=TEST_GUILD_ID, initial_mmr=3000)

        player_repo.update_balance(1, TEST_GUILD_ID, 100)
        player_repo.update_balance(2, TEST_GUILD_ID, 50)
        player_repo.update_balance(3, TEST_GUILD_ID, 0)
        player_repo.update_balance(4, TEST_GUILD_ID, -50)  # Debtor

        eligible = player_repo.get_stimulus_eligible_players(TEST_GUILD_ID)

        # Only non-debtors, excluding top 3, so none eligible (only 3 non-debtors)
        eligible_ids = {p["discord_id"] for p in eligible}
        assert 4 not in eligible_ids  # Debtor excluded
        assert len(eligible_ids) == 0  # Only 3 non-debtors, all in top 3

    def test_get_stimulus_eligible_fewer_than_4_players(self, player_repo):
        """Test stimulus with fewer than 4 non-debtor players returns empty."""
        player_repo.add(discord_id=1, discord_username="Player1", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="Player2", guild_id=TEST_GUILD_ID, initial_mmr=3000)
        player_repo.add(discord_id=3, discord_username="Player3", guild_id=TEST_GUILD_ID, initial_mmr=3000)

        player_repo.update_balance(1, TEST_GUILD_ID, 100)
        player_repo.update_balance(2, TEST_GUILD_ID, 50)
        player_repo.update_balance(3, TEST_GUILD_ID, 10)

        eligible = player_repo.get_stimulus_eligible_players(TEST_GUILD_ID)
        # All 3 are in top 3, so none eligible
        assert len(eligible) == 0


class TestCanProposeWithStimulus:
    """Test proposal creation with stimulus-only scenarios."""

    def test_can_propose_with_only_stimulus_eligible(
        self, disburse_service, player_repo, loan_repo
    ):
        """Test proposal can be created when no debtors but stimulus recipients exist."""
        # Create 5 players, all with non-negative balance
        for i in range(1, 6):
            player_repo.add(discord_id=i, discord_username=f"Player{i}", guild_id=TEST_GUILD_ID, initial_mmr=3000)
            player_repo.update_balance(i, TEST_GUILD_ID, 100 - i * 10)  # 90, 80, 70, 60, 50

        # Add nonprofit fund
        loan_repo.add_to_nonprofit_fund(guild_id=TEST_GUILD_ID, amount=300)

        can, reason = disburse_service.can_propose(guild_id=TEST_GUILD_ID)
        # Should be allowed - 5 non-debtors, 2 eligible (not in top 3)
        assert can
        assert reason == ""

    def test_can_propose_no_eligible_recipients(
        self, disburse_service, player_repo, loan_repo
    ):
        """Test proposal blocked when no debtors and no stimulus-eligible players."""
        # Create only 3 players, all non-debtors (all in top 3)
        for i in range(1, 4):
            player_repo.add(discord_id=i, discord_username=f"Player{i}", guild_id=TEST_GUILD_ID, initial_mmr=3000)
            player_repo.update_balance(i, TEST_GUILD_ID, 100)

        # Add nonprofit fund
        loan_repo.add_to_nonprofit_fund(guild_id=TEST_GUILD_ID, amount=300)

        can, reason = disburse_service.can_propose(guild_id=TEST_GUILD_ID)
        # Should be blocked - only 3 non-debtors, all in top 3
        assert not can
        assert reason == "no_eligible_recipients"


class TestStimulusExecution:
    """Test full stimulus execution flow."""

    def test_execute_stimulus_disbursement(
        self, disburse_service, player_repo, loan_repo
    ):
        """Test full stimulus disbursement execution."""
        # Create 6 players - varied balances
        for i in range(1, 7):
            player_repo.add(discord_id=i, discord_username=f"Player{i}", guild_id=TEST_GUILD_ID, initial_mmr=3000)

        player_repo.update_balance(1, TEST_GUILD_ID, 500)   # Top 1
        player_repo.update_balance(2, TEST_GUILD_ID, 300)   # Top 2
        player_repo.update_balance(3, TEST_GUILD_ID, 100)   # Top 3
        player_repo.update_balance(4, TEST_GUILD_ID, 50)    # Eligible
        player_repo.update_balance(5, TEST_GUILD_ID, 30)    # Eligible
        player_repo.update_balance(6, TEST_GUILD_ID, 10)    # Eligible

        # Add nonprofit fund
        loan_repo.add_to_nonprofit_fund(guild_id=TEST_GUILD_ID, amount=300)

        # Create proposal
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Vote for stimulus (quorum = 3 for 6 players at 40%)
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1, method="stimulus")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=2, method="stimulus")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=3, method="stimulus")

        result = disburse_service.execute_disbursement(guild_id=TEST_GUILD_ID)

        assert result["success"]
        assert result["method"] == "stimulus"
        assert result["recipient_count"] == 3  # Players 4, 5, 6
        assert result["total_disbursed"] == 300

        # Check balances updated
        assert player_repo.get_balance(4, TEST_GUILD_ID) == 50 + 100  # 150
        assert player_repo.get_balance(5, TEST_GUILD_ID) == 30 + 100  # 130
        assert player_repo.get_balance(6, TEST_GUILD_ID) == 10 + 100  # 110

        # Top 3 unchanged
        assert player_repo.get_balance(1, TEST_GUILD_ID) == 500
        assert player_repo.get_balance(2, TEST_GUILD_ID) == 300
        assert player_repo.get_balance(3, TEST_GUILD_ID) == 100


class TestLotteryDistribution:
    """Test lottery distribution calculation."""

    def test_lottery_distribution_basic(self, disburse_service):
        """Test that lottery picks one random player who gets all funds."""
        players = [
            {"discord_id": 1001},
            {"discord_id": 1002},
            {"discord_id": 1003},
        ]
        distributions = disburse_service._calculate_lottery_distribution(100, players)

        assert len(distributions) == 1
        assert distributions[0][1] == 100  # Winner takes all
        assert distributions[0][0] in [1001, 1002, 1003]

    def test_lottery_distribution_single_player(self, disburse_service):
        """Test lottery with single player."""
        players = [{"discord_id": 1001}]
        distributions = disburse_service._calculate_lottery_distribution(100, players)

        assert len(distributions) == 1
        assert distributions[0] == (1001, 100)

    def test_lottery_distribution_empty(self, disburse_service):
        """Test lottery with no players."""
        distributions = disburse_service._calculate_lottery_distribution(100, [])
        assert distributions == []


class TestSocialSecurityDistribution:
    """Test social security distribution calculation."""

    def test_social_security_distribution_basic(self, disburse_service):
        """Test proportional distribution by games played."""
        players = [
            {"discord_id": 1001, "games_played": 30},  # 30% of 100 games
            {"discord_id": 1002, "games_played": 50},  # 50% of 100 games
            {"discord_id": 1003, "games_played": 20},  # 20% of 100 games
        ]
        distributions = disburse_service._calculate_social_security_distribution(100, players)

        amounts = {d[0]: d[1] for d in distributions}
        # Should be roughly proportional
        assert sum(amounts.values()) == 100
        # Player with 50 games should get most
        assert amounts[1002] >= amounts[1001]
        assert amounts[1002] >= amounts[1003]

    def test_social_security_distribution_single_player(self, disburse_service):
        """Test social security with single player."""
        players = [{"discord_id": 1001, "games_played": 10}]
        distributions = disburse_service._calculate_social_security_distribution(100, players)

        assert len(distributions) == 1
        assert distributions[0] == (1001, 100)

    def test_social_security_distribution_empty(self, disburse_service):
        """Test social security with no players."""
        distributions = disburse_service._calculate_social_security_distribution(100, [])
        assert distributions == []

    def test_social_security_distribution_zero_games(self, disburse_service):
        """Test social security when all players have zero games (shouldn't happen)."""
        players = [
            {"discord_id": 1001, "games_played": 0},
            {"discord_id": 1002, "games_played": 0},
        ]
        distributions = disburse_service._calculate_social_security_distribution(100, players)
        assert distributions == []

    def test_social_security_distributes_all_funds(self, disburse_service):
        """Test that all funds are distributed (no cap)."""
        players = [
            {"discord_id": 1001, "games_played": 10},
            {"discord_id": 1002, "games_played": 10},
        ]
        distributions = disburse_service._calculate_social_security_distribution(100, players)

        total = sum(d[1] for d in distributions)
        assert total == 100


class TestCancelDisbursement:
    """Test cancel vote handling."""

    def test_cancel_wins_resets_proposal(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that cancel vote resets proposal instead of distributing."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Vote for cancel
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="cancel")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1004, method="cancel")

        result = disburse_service.execute_disbursement(guild_id=TEST_GUILD_ID)

        assert result["success"]
        assert result["method"] == "cancel"
        assert result["cancelled"] is True
        assert result["total_disbursed"] == 0
        assert "cancelled" in result["message"].lower() or "remain" in result["message"].lower()

    def test_cancel_tiebreaker_loses(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that cancel loses all ties (lowest priority in tiebreaker)."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Vote in a tie between even and cancel
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="even")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1004, method="cancel")

        quorum_reached, winner = disburse_service.check_quorum(guild_id=TEST_GUILD_ID)
        assert quorum_reached
        assert winner == "even"  # Even wins tiebreaker

    def test_cancel_preserves_fund(
        self, disburse_service, loan_repo, setup_players, setup_nonprofit_fund
    ):
        """Test that cancel preserves funds in nonprofit."""
        initial_fund = loan_repo.get_nonprofit_fund(guild_id=TEST_GUILD_ID)

        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1003, method="cancel")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1004, method="cancel")

        disburse_service.execute_disbursement(guild_id=TEST_GUILD_ID)

        final_fund = loan_repo.get_nonprofit_fund(guild_id=TEST_GUILD_ID)
        assert final_fund == initial_fund


class TestLotteryExecution:
    """Test full lottery execution flow."""

    def test_execute_lottery_disbursement(
        self, disburse_service, player_repo, loan_repo
    ):
        """Test full lottery disbursement execution."""
        # Create 5 players
        for i in range(1, 6):
            player_repo.add(discord_id=i, discord_username=f"Player{i}", guild_id=TEST_GUILD_ID, initial_mmr=3000)
            player_repo.update_balance(i, TEST_GUILD_ID, 50)

        # Add nonprofit fund
        loan_repo.add_to_nonprofit_fund(guild_id=TEST_GUILD_ID, amount=300)

        # Create proposal
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Vote for lottery (quorum = 2 for 5 players at 40%)
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1, method="lottery")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=2, method="lottery")

        result = disburse_service.execute_disbursement(guild_id=TEST_GUILD_ID)

        assert result["success"]
        assert result["method"] == "lottery"
        assert result["recipient_count"] == 1  # Only one winner
        assert result["total_disbursed"] == 300  # Winner gets all

        # Verify one player has 350 (50 + 300) and others still have 50
        balances = [player_repo.get_balance(i, TEST_GUILD_ID) for i in range(1, 6)]
        assert 350 in balances
        assert balances.count(50) == 4


class TestSocialSecurityExecution:
    """Test full social security execution flow."""

    def test_execute_social_security_disbursement(
        self, disburse_service, player_repo, loan_repo
    ):
        """Test full social security disbursement execution."""
        # Create 4 players with varying game counts
        for i in range(1, 5):
            player_repo.add(discord_id=i, discord_username=f"Player{i}", guild_id=TEST_GUILD_ID, initial_mmr=3000)
            player_repo.update_balance(i, TEST_GUILD_ID, 50)

        # Set wins/losses for game counts
        # Use repository directly to set wins/losses
        with player_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("UPDATE players SET wins = 20, losses = 10 WHERE discord_id = 1 AND guild_id = ?", (TEST_GUILD_ID,))
            cursor.execute("UPDATE players SET wins = 10, losses = 10 WHERE discord_id = 2 AND guild_id = ?", (TEST_GUILD_ID,))
            cursor.execute("UPDATE players SET wins = 5, losses = 5 WHERE discord_id = 3 AND guild_id = ?", (TEST_GUILD_ID,))
            cursor.execute("UPDATE players SET wins = 0, losses = 0 WHERE discord_id = 4 AND guild_id = ?", (TEST_GUILD_ID,))

        # Add nonprofit fund
        loan_repo.add_to_nonprofit_fund(guild_id=TEST_GUILD_ID, amount=300)

        # Create proposal
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Vote for social security (quorum = 2 for 4 players at 40%)
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=1, method="social_security")
        disburse_service.add_vote(guild_id=TEST_GUILD_ID, discord_id=2, method="social_security")

        result = disburse_service.execute_disbursement(guild_id=TEST_GUILD_ID)

        assert result["success"]
        assert result["method"] == "social_security"
        assert result["recipient_count"] == 3  # Player 4 excluded (0 games)
        assert result["total_disbursed"] == 300  # All funds distributed

        # Player 1 (30 games, 50%) should get most
        # Player 2 (20 games, 33%) middle
        # Player 3 (10 games, 17%) least
        # Player 4 (0 games) excluded
        bal1 = player_repo.get_balance(1, TEST_GUILD_ID)
        bal2 = player_repo.get_balance(2, TEST_GUILD_ID)
        bal3 = player_repo.get_balance(3, TEST_GUILD_ID)
        bal4 = player_repo.get_balance(4, TEST_GUILD_ID)

        assert bal1 > bal2 > bal3
        assert bal4 == 50  # Unchanged (not eligible)


class TestGetIndividualVotes:
    """Test get_individual_votes repository method."""

    def test_get_individual_votes_basic(
        self, disburse_repo, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test retrieving individual vote records."""
        # Create proposal
        proposal = disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Add votes
        disburse_service.add_vote(TEST_GUILD_ID, 1003, "even")
        disburse_service.add_vote(TEST_GUILD_ID, 1004, "proportional")
        disburse_service.add_vote(TEST_GUILD_ID, 1005, "neediest")

        # Get individual votes
        votes = disburse_repo.get_individual_votes(TEST_GUILD_ID)

        assert len(votes) == 3
        assert all("discord_id" in v for v in votes)
        assert all("vote_method" in v for v in votes)
        assert all("voted_at" in v for v in votes)

        # Check specific votes
        vote_map = {v["discord_id"]: v["vote_method"] for v in votes}
        assert vote_map[1003] == "even"
        assert vote_map[1004] == "proportional"
        assert vote_map[1005] == "neediest"

    def test_get_individual_votes_no_proposal(self, disburse_repo):
        """Test getting votes when no active proposal."""
        votes = disburse_repo.get_individual_votes(TEST_GUILD_ID)
        assert votes == []

    def test_get_individual_votes_chronological(
        self, disburse_repo, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test votes are returned in chronological order."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Add votes with slight delays to ensure different timestamps
        disburse_service.add_vote(TEST_GUILD_ID, 1003, "even")
        time.sleep(0.01)
        disburse_service.add_vote(TEST_GUILD_ID, 1004, "proportional")
        time.sleep(0.01)
        disburse_service.add_vote(TEST_GUILD_ID, 1005, "neediest")

        votes = disburse_repo.get_individual_votes(TEST_GUILD_ID)

        # Should be chronological
        assert votes[0]["discord_id"] == 1003
        assert votes[1]["discord_id"] == 1004
        assert votes[2]["discord_id"] == 1005

    def test_get_individual_votes_vote_change(
        self, disburse_repo, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that vote changes are reflected correctly."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        # Vote even, then change to proportional
        disburse_service.add_vote(TEST_GUILD_ID, 1003, "even")
        disburse_service.add_vote(TEST_GUILD_ID, 1003, "proportional")

        votes = disburse_repo.get_individual_votes(TEST_GUILD_ID)

        # Should still have only 1 vote (changed vote)
        assert len(votes) == 1
        assert votes[0]["discord_id"] == 1003
        assert votes[0]["vote_method"] == "proportional"

    def test_get_individual_votes_no_votes_yet(
        self, disburse_repo, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test getting votes when proposal exists but no votes yet."""
        disburse_service.create_proposal(guild_id=TEST_GUILD_ID)

        votes = disburse_repo.get_individual_votes(TEST_GUILD_ID)
        assert votes == []
