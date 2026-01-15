"""
Tests for DisburseService - nonprofit fund distribution voting and distribution.
"""

import pytest
import time

from services.disburse_service import DisburseService
from services.loan_service import LoanRepository
from repositories.disburse_repository import DisburseRepository
from repositories.player_repository import PlayerRepository


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
    player_repo.add(
        discord_id=1001, discord_username="Debtor1", initial_mmr=3000
    )
    player_repo.add(
        discord_id=1002, discord_username="Debtor2", initial_mmr=3000
    )
    player_repo.add(
        discord_id=1003, discord_username="Voter1", initial_mmr=3000
    )
    player_repo.add(
        discord_id=1004, discord_username="Voter2", initial_mmr=3000
    )
    player_repo.add(
        discord_id=1005, discord_username="Voter3", initial_mmr=3000
    )

    # Set balances
    player_repo.update_balance(1001, -100)  # Debtor1: -100
    player_repo.update_balance(1002, -50)   # Debtor2: -50
    player_repo.update_balance(1003, 100)   # Voter1: +100
    player_repo.update_balance(1004, 100)   # Voter2: +100
    player_repo.update_balance(1005, 100)   # Voter3: +100


@pytest.fixture
def setup_nonprofit_fund(loan_repo):
    """Add funds to the nonprofit fund."""
    loan_repo.add_to_nonprofit_fund(guild_id=None, amount=300)


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
        can, reason = disburse_service.can_propose(guild_id=None)
        assert not can
        assert reason.startswith("insufficient_fund:")

    def test_can_propose_success(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test proposal can be created when conditions are met."""
        can, reason = disburse_service.can_propose(guild_id=None)
        assert can
        assert reason == ""

    def test_can_propose_no_eligible_recipients(
        self, disburse_service, player_repo, setup_nonprofit_fund
    ):
        """Test proposal blocked when no debtors and insufficient non-debtors for stimulus."""
        # Create only 1 player with positive balance (not enough for stimulus)
        player_repo.add(discord_id=9999, discord_username="RichGuy", initial_mmr=3000)
        player_repo.update_balance(9999, 1000)

        can, reason = disburse_service.can_propose(guild_id=None)
        assert not can
        assert reason == "no_eligible_recipients"

    def test_create_proposal(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test proposal creation."""
        proposal = disburse_service.create_proposal(guild_id=None)

        assert proposal is not None
        assert proposal.fund_amount == 300
        assert proposal.status == "active"
        assert proposal.quorum_required >= 1  # At least 1 for 5 players

    def test_cannot_create_duplicate_proposal(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that duplicate proposals are blocked."""
        disburse_service.create_proposal(guild_id=None)

        can, reason = disburse_service.can_propose(guild_id=None)
        assert not can
        assert reason == "active_proposal_exists"

    def test_add_vote(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test voting on a proposal."""
        disburse_service.create_proposal(guild_id=None)

        result = disburse_service.add_vote(guild_id=None, discord_id=1003, method="even")

        assert result["votes"]["even"] == 1
        assert result["total_votes"] == 1
        assert not result["quorum_reached"]

    def test_vote_change(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that a player can change their vote."""
        disburse_service.create_proposal(guild_id=None)

        # Vote even
        disburse_service.add_vote(guild_id=None, discord_id=1003, method="even")
        # Change to proportional
        result = disburse_service.add_vote(guild_id=None, discord_id=1003, method="proportional")

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
        proposal = disburse_service.create_proposal(guild_id=None)

        # 5 players, 40% quorum = 2 votes needed
        assert proposal.quorum_required == 2

    def test_quorum_reached(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test quorum detection."""
        disburse_service.create_proposal(guild_id=None)

        # Add 2 votes (40% of 5 players)
        disburse_service.add_vote(guild_id=None, discord_id=1003, method="even")
        result = disburse_service.add_vote(guild_id=None, discord_id=1004, method="even")

        assert result["quorum_reached"]

    def test_tie_breaker_even_wins(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that ties are broken in favor of even split."""
        disburse_service.create_proposal(guild_id=None)

        # Add votes that result in a tie
        disburse_service.add_vote(guild_id=None, discord_id=1003, method="even")
        disburse_service.add_vote(guild_id=None, discord_id=1004, method="proportional")

        quorum_reached, winner = disburse_service.check_quorum(guild_id=None)
        assert quorum_reached
        assert winner == "even"  # Tie breaker

    def test_execute_disbursement(
        self, disburse_service, player_repo, setup_players, setup_nonprofit_fund
    ):
        """Test full disbursement execution."""
        disburse_service.create_proposal(guild_id=None)

        # Vote for even split
        disburse_service.add_vote(guild_id=None, discord_id=1003, method="even")
        disburse_service.add_vote(guild_id=None, discord_id=1004, method="even")

        result = disburse_service.execute_disbursement(guild_id=None)

        assert result["success"]
        assert result["method"] == "even"
        assert result["total_disbursed"] > 0
        assert result["recipient_count"] == 2  # Two debtors

        # Check that debtors received funds
        debtor1_balance = player_repo.get_balance(1001)
        debtor2_balance = player_repo.get_balance(1002)
        assert debtor1_balance > -100  # Was -100
        assert debtor2_balance > -50   # Was -50

    def test_disbursement_marks_complete(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test that disbursement marks proposal as completed."""
        disburse_service.create_proposal(guild_id=None)
        disburse_service.add_vote(guild_id=None, discord_id=1003, method="even")
        disburse_service.add_vote(guild_id=None, discord_id=1004, method="even")

        disburse_service.execute_disbursement(guild_id=None)

        # Should be able to create a new proposal now
        can, reason = disburse_service.can_propose(guild_id=None)
        # Note: might fail due to no more funds, but not due to active proposal
        assert reason != "active_proposal_exists"


class TestResetProposal:
    """Test proposal reset functionality."""

    def test_reset_proposal(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test resetting an active proposal."""
        disburse_service.create_proposal(guild_id=None)
        disburse_service.add_vote(guild_id=None, discord_id=1003, method="even")

        success = disburse_service.reset_proposal(guild_id=None)
        assert success

        # Should be able to create a new proposal
        can, reason = disburse_service.can_propose(guild_id=None)
        assert can

    def test_reset_no_proposal(self, disburse_service):
        """Test resetting when no proposal exists."""
        success = disburse_service.reset_proposal(guild_id=None)
        assert not success


class TestDisbursementHistory:
    """Test disbursement history tracking."""

    def test_get_last_disbursement(
        self, disburse_service, setup_players, setup_nonprofit_fund
    ):
        """Test retrieving last disbursement info."""
        disburse_service.create_proposal(guild_id=None)
        disburse_service.add_vote(guild_id=None, discord_id=1003, method="even")
        disburse_service.add_vote(guild_id=None, discord_id=1004, method="even")
        disburse_service.execute_disbursement(guild_id=None)

        last = disburse_service.get_last_disbursement(guild_id=None)

        assert last is not None
        assert last["method"] == "even"
        assert last["total_amount"] > 0
        assert last["recipient_count"] == 2
        assert len(last["recipients"]) == 2

    def test_no_history(self, disburse_service):
        """Test when no disbursement history exists."""
        last = disburse_service.get_last_disbursement(guild_id=None)
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


class TestStimulusEligibility:
    """Test stimulus eligibility in repository."""

    def test_get_stimulus_eligible_excludes_top_3(self, player_repo):
        """Test that top 3 by balance are excluded."""
        # Create 6 players with varying balances
        player_repo.add(discord_id=1, discord_username="Rich1", initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="Rich2", initial_mmr=3000)
        player_repo.add(discord_id=3, discord_username="Rich3", initial_mmr=3000)
        player_repo.add(discord_id=4, discord_username="Mid1", initial_mmr=3000)
        player_repo.add(discord_id=5, discord_username="Mid2", initial_mmr=3000)
        player_repo.add(discord_id=6, discord_username="Poor1", initial_mmr=3000)

        player_repo.update_balance(1, 1000)  # Top 1
        player_repo.update_balance(2, 500)   # Top 2
        player_repo.update_balance(3, 200)   # Top 3
        player_repo.update_balance(4, 50)    # Eligible
        player_repo.update_balance(5, 10)    # Eligible
        player_repo.update_balance(6, 0)     # Eligible (zero balance is non-negative)

        eligible = player_repo.get_stimulus_eligible_players()

        # Should only return players 4, 5, 6
        eligible_ids = {p["discord_id"] for p in eligible}
        assert eligible_ids == {4, 5, 6}

    def test_get_stimulus_eligible_excludes_debtors(self, player_repo):
        """Test that players with negative balance are excluded."""
        player_repo.add(discord_id=1, discord_username="Rich", initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="Mid", initial_mmr=3000)
        player_repo.add(discord_id=3, discord_username="Zero", initial_mmr=3000)
        player_repo.add(discord_id=4, discord_username="Debtor", initial_mmr=3000)

        player_repo.update_balance(1, 100)
        player_repo.update_balance(2, 50)
        player_repo.update_balance(3, 0)
        player_repo.update_balance(4, -50)  # Debtor

        eligible = player_repo.get_stimulus_eligible_players()

        # Only non-debtors, excluding top 3, so none eligible (only 3 non-debtors)
        eligible_ids = {p["discord_id"] for p in eligible}
        assert 4 not in eligible_ids  # Debtor excluded
        assert len(eligible_ids) == 0  # Only 3 non-debtors, all in top 3

    def test_get_stimulus_eligible_fewer_than_4_players(self, player_repo):
        """Test stimulus with fewer than 4 non-debtor players returns empty."""
        player_repo.add(discord_id=1, discord_username="Player1", initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="Player2", initial_mmr=3000)
        player_repo.add(discord_id=3, discord_username="Player3", initial_mmr=3000)

        player_repo.update_balance(1, 100)
        player_repo.update_balance(2, 50)
        player_repo.update_balance(3, 10)

        eligible = player_repo.get_stimulus_eligible_players()
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
            player_repo.add(discord_id=i, discord_username=f"Player{i}", initial_mmr=3000)
            player_repo.update_balance(i, 100 - i * 10)  # 90, 80, 70, 60, 50

        # Add nonprofit fund
        loan_repo.add_to_nonprofit_fund(guild_id=None, amount=300)

        can, reason = disburse_service.can_propose(guild_id=None)
        # Should be allowed - 5 non-debtors, 2 eligible (not in top 3)
        assert can
        assert reason == ""

    def test_can_propose_no_eligible_recipients(
        self, disburse_service, player_repo, loan_repo
    ):
        """Test proposal blocked when no debtors and no stimulus-eligible players."""
        # Create only 3 players, all non-debtors (all in top 3)
        for i in range(1, 4):
            player_repo.add(discord_id=i, discord_username=f"Player{i}", initial_mmr=3000)
            player_repo.update_balance(i, 100)

        # Add nonprofit fund
        loan_repo.add_to_nonprofit_fund(guild_id=None, amount=300)

        can, reason = disburse_service.can_propose(guild_id=None)
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
            player_repo.add(discord_id=i, discord_username=f"Player{i}", initial_mmr=3000)

        player_repo.update_balance(1, 500)   # Top 1
        player_repo.update_balance(2, 300)   # Top 2
        player_repo.update_balance(3, 100)   # Top 3
        player_repo.update_balance(4, 50)    # Eligible
        player_repo.update_balance(5, 30)    # Eligible
        player_repo.update_balance(6, 10)    # Eligible

        # Add nonprofit fund
        loan_repo.add_to_nonprofit_fund(guild_id=None, amount=300)

        # Create proposal
        disburse_service.create_proposal(guild_id=None)

        # Vote for stimulus (quorum = 3 for 6 players at 40%)
        disburse_service.add_vote(guild_id=None, discord_id=1, method="stimulus")
        disburse_service.add_vote(guild_id=None, discord_id=2, method="stimulus")
        disburse_service.add_vote(guild_id=None, discord_id=3, method="stimulus")

        result = disburse_service.execute_disbursement(guild_id=None)

        assert result["success"]
        assert result["method"] == "stimulus"
        assert result["recipient_count"] == 3  # Players 4, 5, 6
        assert result["total_disbursed"] == 300

        # Check balances updated
        assert player_repo.get_balance(4) == 50 + 100  # 150
        assert player_repo.get_balance(5) == 30 + 100  # 130
        assert player_repo.get_balance(6) == 10 + 100  # 110

        # Top 3 unchanged
        assert player_repo.get_balance(1) == 500
        assert player_repo.get_balance(2) == 300
        assert player_repo.get_balance(3) == 100
