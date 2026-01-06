"""
Tests for prediction market functionality.
"""

import time

import pytest

from repositories.player_repository import PlayerRepository
from repositories.prediction_repository import PredictionRepository
from services.prediction_service import PredictionService


@pytest.fixture
def prediction_repo(repo_db_path):
    """Create a PredictionRepository with initialized schema."""
    return PredictionRepository(repo_db_path)


@pytest.fixture
def player_repo(repo_db_path):
    """Create a PlayerRepository with initialized schema."""
    return PlayerRepository(repo_db_path)


@pytest.fixture
def prediction_service(prediction_repo, player_repo):
    """Create a PredictionService."""
    return PredictionService(
        prediction_repo=prediction_repo,
        player_repo=player_repo,
        admin_user_ids=[999],  # Admin user ID for tests
    )


@pytest.fixture
def registered_player(player_repo):
    """Create a registered player with balance."""
    player_repo.add(
        discord_id=123,
        discord_username="TestPlayer",
        initial_mmr=3000,
    )
    player_repo.update_balance(123, 100)  # Give 100 coins
    return 123


@pytest.fixture
def registered_player2(player_repo):
    """Create a second registered player with balance."""
    player_repo.add(
        discord_id=456,
        discord_username="TestPlayer2",
        initial_mmr=3000,
    )
    player_repo.update_balance(456, 100)
    return 456


@pytest.fixture
def registered_player3(player_repo):
    """Create a third registered player with balance."""
    player_repo.add(
        discord_id=789,
        discord_username="TestPlayer3",
        initial_mmr=3000,
    )
    player_repo.update_balance(789, 100)
    return 789


class TestPredictionRepository:
    """Tests for PredictionRepository."""

    def test_create_prediction(self, prediction_repo):
        """Test creating a prediction."""
        closes_at = int(time.time()) + 3600  # 1 hour from now
        pred_id = prediction_repo.create_prediction(
            guild_id=1,
            creator_id=123,
            question="Will it rain tomorrow?",
            closes_at=closes_at,
        )
        assert pred_id > 0

        pred = prediction_repo.get_prediction(pred_id)
        assert pred is not None
        assert pred["question"] == "Will it rain tomorrow?"
        assert pred["creator_id"] == 123
        assert pred["status"] == "open"
        assert pred["closes_at"] == closes_at

    def test_get_active_predictions(self, prediction_repo):
        """Test getting active predictions."""
        closes_at = int(time.time()) + 3600
        prediction_repo.create_prediction(1, 123, "Q1", closes_at)
        prediction_repo.create_prediction(1, 123, "Q2", closes_at)
        prediction_repo.create_prediction(2, 123, "Q3", closes_at)  # Different guild

        active = prediction_repo.get_active_predictions(1)
        assert len(active) == 2

    def test_place_bet_atomic(self, prediction_repo, player_repo):
        """Test atomic bet placement."""
        # Create player with balance
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 50)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        result = prediction_repo.place_bet_atomic(
            prediction_id=pred_id,
            discord_id=123,
            position="yes",
            amount=10,
        )

        assert result["bet_id"] > 0
        assert result["amount"] == 10
        assert result["position"] == "yes"
        assert result["new_balance"] == 40
        assert result["yes_total"] == 10
        assert result["no_total"] == 0

    def test_place_bet_insufficient_balance(self, prediction_repo, player_repo):
        """Test bet placement with insufficient balance."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 5)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        with pytest.raises(ValueError, match="Insufficient balance"):
            prediction_repo.place_bet_atomic(pred_id, 123, "yes", 10)

    def test_place_bet_opposite_side_rejected(self, prediction_repo, player_repo):
        """Test that betting on opposite side is rejected."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        # First bet on YES
        prediction_repo.place_bet_atomic(pred_id, 123, "yes", 10)

        # Try to bet on NO - should fail
        with pytest.raises(ValueError, match="already have a position"):
            prediction_repo.place_bet_atomic(pred_id, 123, "no", 10)

    def test_place_bet_same_side_allowed(self, prediction_repo, player_repo):
        """Test that adding to same side is allowed."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        # First bet on YES
        prediction_repo.place_bet_atomic(pred_id, 123, "yes", 10)

        # Add more to YES - should work
        result = prediction_repo.place_bet_atomic(pred_id, 123, "yes", 20)
        assert result["yes_total"] == 30

    def test_resolution_votes(self, prediction_repo):
        """Test resolution voting."""
        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 123, "Test?", closes_at)

        # Add votes
        result1 = prediction_repo.add_resolution_vote(pred_id, 1, "yes", False)
        assert result1["yes_count"] == 1
        assert result1["no_count"] == 0

        result2 = prediction_repo.add_resolution_vote(pred_id, 2, "yes", False)
        assert result2["yes_count"] == 2

        result3 = prediction_repo.add_resolution_vote(pred_id, 3, "no", False)
        assert result3["yes_count"] == 2
        assert result3["no_count"] == 1

    def test_resolution_vote_change_rejected(self, prediction_repo):
        """Test that changing vote is rejected."""
        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 123, "Test?", closes_at)

        prediction_repo.add_resolution_vote(pred_id, 1, "yes", False)

        with pytest.raises(ValueError, match="already voted"):
            prediction_repo.add_resolution_vote(pred_id, 1, "no", False)

    def test_settle_prediction_bets(self, prediction_repo, player_repo):
        """Test settling prediction bets."""
        # Create players
        player_repo.add(discord_id=1, discord_username="P1", initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="P2", initial_mmr=3000)
        player_repo.update_balance(1, 100)
        player_repo.update_balance(2, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        # Player 1 bets YES
        prediction_repo.place_bet_atomic(pred_id, 1, "yes", 30)
        # Player 2 bets NO
        prediction_repo.place_bet_atomic(pred_id, 2, "no", 70)

        # Settle with YES winning
        result = prediction_repo.settle_prediction_bets(pred_id, "yes")

        assert len(result["winners"]) == 1
        assert len(result["losers"]) == 1
        assert result["total_pool"] == 100

        # Player 1 should have won the entire pool
        winner = result["winners"][0]
        assert winner["discord_id"] == 1
        assert winner["payout"] == 100  # Entire pool

        # Check balances updated
        p1_balance = player_repo.get_balance(1)
        p2_balance = player_repo.get_balance(2)
        assert p1_balance == 70 + 100  # 70 remaining + 100 payout
        assert p2_balance == 30  # 100 - 70 bet

    def test_refund_prediction_bets(self, prediction_repo, player_repo):
        """Test refunding prediction bets."""
        player_repo.add(discord_id=1, discord_username="P1", initial_mmr=3000)
        player_repo.update_balance(1, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        prediction_repo.place_bet_atomic(pred_id, 1, "yes", 50)
        assert player_repo.get_balance(1) == 50  # 100 - 50

        result = prediction_repo.refund_prediction_bets(pred_id)
        assert result["total_refunded"] == 50
        assert player_repo.get_balance(1) == 100  # Refunded


class TestPredictionService:
    """Tests for PredictionService."""

    def test_create_prediction(self, prediction_service, registered_player):
        """Test creating a prediction via service."""
        closes_at = int(time.time()) + 3600
        result = prediction_service.create_prediction(
            guild_id=1,
            creator_id=registered_player,
            question="Will it rain?",
            closes_at=closes_at,
        )

        assert result["prediction_id"] > 0
        assert result["question"] == "Will it rain?"

    def test_create_prediction_short_question(self, prediction_service, registered_player):
        """Test that short questions are rejected."""
        closes_at = int(time.time()) + 3600
        with pytest.raises(ValueError, match="at least 5 characters"):
            prediction_service.create_prediction(1, registered_player, "Hi?", closes_at)

    def test_create_prediction_past_close(self, prediction_service, registered_player):
        """Test that past close times are rejected."""
        closes_at = int(time.time()) - 100  # In the past
        with pytest.raises(ValueError, match="future"):
            prediction_service.create_prediction(1, registered_player, "Test question?", closes_at)

    def test_place_bet(self, prediction_service, registered_player):
        """Test placing a bet via service."""
        closes_at = int(time.time()) + 3600
        result = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = result["prediction_id"]

        bet_result = prediction_service.place_bet(pred_id, registered_player, "yes", 10)

        assert bet_result["amount"] == 10
        assert bet_result["position"] == "yes"
        assert "odds" in bet_result

    def test_calculate_odds(self, prediction_service):
        """Test odds calculation."""
        odds = prediction_service.calculate_odds(50, 50)
        assert odds["yes"] == 2.0
        assert odds["no"] == 2.0

        odds = prediction_service.calculate_odds(25, 75)
        assert odds["yes"] == 4.0
        assert odds["no"] == pytest.approx(1.33, rel=0.01)

    def test_resolution_voting(
        self, prediction_service, registered_player, registered_player2, registered_player3
    ):
        """Test resolution voting flow."""
        closes_at = int(time.time()) - 1  # Already closed
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at + 100)
        pred_id = pred["prediction_id"]

        # Manually set closes_at to past for testing
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")

        # Force closes_at to be in the past for the vote check
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 1, pred_id),
            )

        # Vote 1 - not enough
        result1 = prediction_service.add_resolution_vote(pred_id, registered_player, "yes")
        assert not result1["can_resolve"]
        assert result1["yes_count"] == 1

        # Vote 2 - not enough
        result2 = prediction_service.add_resolution_vote(pred_id, registered_player2, "yes")
        assert not result2["can_resolve"]
        assert result2["yes_count"] == 2

        # Vote 3 - should be enough
        result3 = prediction_service.add_resolution_vote(pred_id, registered_player3, "yes")
        assert result3["can_resolve"]
        assert result3["yes_count"] == 3

    def test_admin_vote_resolves_immediately(self, prediction_service, registered_player):
        """Test that admin vote immediately allows resolution."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Set to locked and past close time
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 1, pred_id),
            )

        # Admin vote (user 999 is admin)
        result = prediction_service.add_resolution_vote(pred_id, 999, "yes")
        assert result["can_resolve"]
        assert result["is_admin"]

    def test_cancel_requires_admin(self, prediction_service, registered_player):
        """Test that cancellation requires admin."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, registered_player, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Non-admin should fail
        with pytest.raises(ValueError, match="Only admins"):
            prediction_service.cancel(pred_id, registered_player)

        # Admin should succeed
        result = prediction_service.cancel(pred_id, 999)
        assert result["prediction_id"] == pred_id

    def test_full_prediction_flow(
        self, prediction_service, player_repo, registered_player, registered_player2
    ):
        """Test complete prediction flow: create -> bet -> resolve -> settle."""
        # Create prediction
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Will the test pass?", closes_at)
        pred_id = pred["prediction_id"]

        # Place bets
        prediction_service.place_bet(pred_id, registered_player, "yes", 30)
        prediction_service.place_bet(pred_id, registered_player2, "no", 20)

        # Check odds
        odds = prediction_service.get_odds(pred_id)
        assert odds["yes_total"] == 30
        assert odds["no_total"] == 20
        assert odds["total_pool"] == 50

        # Set to locked and past close time for resolution
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 1, pred_id),
            )

        # Resolve (admin can do it in one vote)
        settlement = prediction_service.resolve(pred_id, "yes", 999)

        assert settlement["outcome"] == "yes"
        assert len(settlement["winners"]) == 1
        assert len(settlement["losers"]) == 1

        # Winner should have the pool
        winner = settlement["winners"][0]
        assert winner["discord_id"] == registered_player
        assert winner["payout"] == 50

        # Check final balances
        p1_balance = player_repo.get_balance(registered_player)
        p2_balance = player_repo.get_balance(registered_player2)
        assert p1_balance == 70 + 50  # 100 - 30 bet + 50 payout = 120
        assert p2_balance == 80  # 100 - 20 bet


class TestPredictionCornerCases:
    """Corner case tests for prediction markets."""

    def test_bet_on_closed_prediction(self, prediction_repo, player_repo):
        """Test that betting after close time is rejected."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        # Create prediction that closes immediately
        closes_at = int(time.time()) - 1  # Already closed
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        with pytest.raises(ValueError, match="Betting period has ended"):
            prediction_repo.place_bet_atomic(pred_id, 123, "yes", 10)

    def test_bet_on_resolved_prediction(self, prediction_repo, player_repo):
        """Test that betting on resolved prediction is rejected."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        # Mark as resolved
        prediction_repo.update_prediction_status(pred_id, "resolved")

        with pytest.raises(ValueError, match="Betting is closed"):
            prediction_repo.place_bet_atomic(pred_id, 123, "yes", 10)

    def test_bet_on_cancelled_prediction(self, prediction_repo, player_repo):
        """Test that betting on cancelled prediction is rejected."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        # Mark as cancelled
        prediction_repo.update_prediction_status(pred_id, "cancelled")

        with pytest.raises(ValueError, match="Betting is closed"):
            prediction_repo.place_bet_atomic(pred_id, 123, "yes", 10)

    def test_bet_with_debt(self, prediction_repo, player_repo):
        """Test that users in debt cannot bet."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, -50)  # In debt

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        with pytest.raises(ValueError, match="cannot place bets while in debt"):
            prediction_repo.place_bet_atomic(pred_id, 123, "yes", 10)

    def test_settlement_no_bets_on_winning_side(self, prediction_repo, player_repo):
        """Test settlement when no one bet on winning side - all get refund."""
        player_repo.add(discord_id=1, discord_username="P1", initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="P2", initial_mmr=3000)
        player_repo.update_balance(1, 100)
        player_repo.update_balance(2, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        # Both bet NO
        prediction_repo.place_bet_atomic(pred_id, 1, "no", 30)
        prediction_repo.place_bet_atomic(pred_id, 2, "no", 20)

        # YES wins - no winners, everyone refunded
        result = prediction_repo.settle_prediction_bets(pred_id, "yes")

        assert len(result["winners"]) == 0
        assert len(result["losers"]) == 2
        assert all(l.get("refunded") for l in result["losers"])

        # Balances should be restored
        assert player_repo.get_balance(1) == 100  # 70 + 30 refund
        assert player_repo.get_balance(2) == 100  # 80 + 20 refund

    def test_settlement_only_one_side_has_bets(self, prediction_repo, player_repo):
        """Test settlement when only winners bet - they get original back."""
        player_repo.add(discord_id=1, discord_username="P1", initial_mmr=3000)
        player_repo.update_balance(1, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        # Only bet YES
        prediction_repo.place_bet_atomic(pred_id, 1, "yes", 50)

        # YES wins
        result = prediction_repo.settle_prediction_bets(pred_id, "yes")

        assert len(result["winners"]) == 1
        assert len(result["losers"]) == 0
        # Winner gets their bet back (pool = bet)
        assert result["winners"][0]["payout"] == 50
        assert player_repo.get_balance(1) == 100  # 50 + 50 payout

    def test_vote_before_close_rejected(self, prediction_service, registered_player):
        """Test that voting before close time is rejected."""
        closes_at = int(time.time()) + 3600  # 1 hour from now
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at)
        pred_id = pred["prediction_id"]

        with pytest.raises(ValueError, match="Cannot vote until betting period closes"):
            prediction_service.add_resolution_vote(pred_id, registered_player, "yes")

    def test_resolve_already_resolved(self, prediction_service):
        """Test that resolving an already resolved prediction fails."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Force to resolved state
        prediction_service.prediction_repo.resolve_prediction(pred_id, "yes", 999)

        with pytest.raises(ValueError, match="already resolved"):
            prediction_service.resolve(pred_id, "no", 999)

    def test_multiple_bets_same_user_same_side(self, prediction_repo, player_repo):
        """Test that multiple bets on same side accumulate correctly."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        prediction_repo.place_bet_atomic(pred_id, 123, "yes", 10)
        prediction_repo.place_bet_atomic(pred_id, 123, "yes", 20)
        prediction_repo.place_bet_atomic(pred_id, 123, "yes", 15)

        user_bet = prediction_repo.get_user_bet_on_prediction(pred_id, 123)
        assert user_bet["total_amount"] == 45
        assert user_bet["bet_count"] == 3

    def test_get_user_active_positions(self, prediction_repo, player_repo):
        """Test getting user's active positions across predictions."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        closes_at = int(time.time()) + 3600
        pred1 = prediction_repo.create_prediction(1, 999, "Question 1?", closes_at)
        pred2 = prediction_repo.create_prediction(1, 999, "Question 2?", closes_at)
        pred3 = prediction_repo.create_prediction(1, 999, "Question 3?", closes_at)

        prediction_repo.place_bet_atomic(pred1, 123, "yes", 10)
        prediction_repo.place_bet_atomic(pred2, 123, "no", 20)
        # No bet on pred3

        positions = prediction_repo.get_user_active_positions(123)
        assert len(positions) == 2
        assert any(p["prediction_id"] == pred1 and p["position"] == "yes" for p in positions)
        assert any(p["prediction_id"] == pred2 and p["position"] == "no" for p in positions)

    def test_prediction_not_found(self, prediction_service, registered_player):
        """Test error handling for non-existent prediction."""
        result = prediction_service.get_prediction(99999)
        assert result is None

        with pytest.raises(ValueError, match="not found"):
            prediction_service.resolve(99999, "yes", 999)

    def test_unregistered_player_bet(self, prediction_repo):
        """Test that unregistered player cannot bet."""
        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        with pytest.raises(ValueError, match="Player not found"):
            prediction_repo.place_bet_atomic(pred_id, 99999, "yes", 10)

    def test_zero_amount_bet_rejected(self, prediction_repo, player_repo):
        """Test that zero amount bet is rejected."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        with pytest.raises(ValueError, match="must be positive"):
            prediction_repo.place_bet_atomic(pred_id, 123, "yes", 0)

    def test_negative_amount_bet_rejected(self, prediction_repo, player_repo):
        """Test that negative amount bet is rejected."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        with pytest.raises(ValueError, match="must be positive"):
            prediction_repo.place_bet_atomic(pred_id, 123, "yes", -10)

    def test_invalid_position_rejected(self, prediction_repo, player_repo):
        """Test that invalid position is rejected."""
        player_repo.add(discord_id=123, discord_username="Test", initial_mmr=3000)
        player_repo.update_balance(123, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        with pytest.raises(ValueError, match="Invalid position"):
            prediction_repo.place_bet_atomic(pred_id, 123, "maybe", 10)

    def test_cancel_already_cancelled(self, prediction_service):
        """Test that cancelling an already cancelled prediction fails."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at)
        pred_id = pred["prediction_id"]

        prediction_service.cancel(pred_id, 999)

        with pytest.raises(ValueError, match="Can only cancel open"):
            prediction_service.cancel(pred_id, 999)

    def test_cancel_resolved_prediction(self, prediction_service):
        """Test that cancelling a resolved prediction fails."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at)
        pred_id = pred["prediction_id"]

        prediction_service.prediction_repo.resolve_prediction(pred_id, "yes", 999)

        with pytest.raises(ValueError, match="Can only cancel open"):
            prediction_service.cancel(pred_id, 999)

    def test_cancel_locked_prediction(self, prediction_service):
        """Test that cancelling a locked prediction fails."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at)
        pred_id = pred["prediction_id"]

        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")

        with pytest.raises(ValueError, match="Can only cancel open"):
            prediction_service.cancel(pred_id, 999)

    def test_proportional_payout_calculation(self, prediction_repo, player_repo):
        """Test that proportional payouts are calculated correctly."""
        player_repo.add(discord_id=1, discord_username="P1", initial_mmr=3000)
        player_repo.add(discord_id=2, discord_username="P2", initial_mmr=3000)
        player_repo.add(discord_id=3, discord_username="P3", initial_mmr=3000)
        player_repo.update_balance(1, 100)
        player_repo.update_balance(2, 100)
        player_repo.update_balance(3, 100)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        # Two winners, one loser
        prediction_repo.place_bet_atomic(pred_id, 1, "yes", 30)  # 30/50 = 60%
        prediction_repo.place_bet_atomic(pred_id, 2, "yes", 20)  # 20/50 = 40%
        prediction_repo.place_bet_atomic(pred_id, 3, "no", 50)

        # Total pool: 100, winner pool: 50
        result = prediction_repo.settle_prediction_bets(pred_id, "yes")

        # P1 should get 60% of 100 = 60, P2 should get 40% of 100 = 40
        winners_by_id = {w["discord_id"]: w for w in result["winners"]}
        assert winners_by_id[1]["payout"] == 60
        assert winners_by_id[2]["payout"] == 40

    def test_prediction_minimum_duration(self, prediction_service, registered_player):
        """Test that minimum duration is enforced."""
        closes_at = int(time.time()) + 30  # Only 30 seconds
        with pytest.raises(ValueError, match="at least 1 minute"):
            prediction_service.create_prediction(1, registered_player, "Too short?", closes_at)

    def test_expired_predictions_auto_locked(self, prediction_service):
        """Test that expired predictions are auto-locked when fetching active list."""
        # Create a prediction that's already expired
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at)
        pred_id = pred["prediction_id"]

        # Manually set closes_at to past
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 100, pred_id),
            )

        # Verify it's still "open" in DB
        pred_before = prediction_service.get_prediction(pred_id)
        assert pred_before["status"] == "open"

        # Fetch active predictions - should trigger auto-lock
        prediction_service.get_active_predictions(1)

        # Now it should be locked
        pred_after = prediction_service.get_prediction(pred_id)
        assert pred_after["status"] == "locked"

    def test_cancelled_predictions_not_in_active_list(self, prediction_service):
        """Test that cancelled predictions don't appear in active list."""
        closes_at = int(time.time()) + 3600
        pred1 = prediction_service.create_prediction(1, 999, "Question 1?", closes_at)
        pred2 = prediction_service.create_prediction(1, 999, "Question 2?", closes_at)

        # Cancel pred1
        prediction_service.cancel(pred1["prediction_id"], 999)

        # Get active predictions
        active = prediction_service.get_active_predictions(1)

        # Only pred2 should be in list
        assert len(active) == 1
        assert active[0]["prediction_id"] == pred2["prediction_id"]

    def test_external_admin_flag_for_resolution(self, prediction_service, registered_player):
        """Test that external is_admin flag overrides internal check."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Set to locked and past close time
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 1, pred_id),
            )

        # Non-admin user (registered_player=123) with is_admin=True should resolve
        result = prediction_service.add_resolution_vote(
            pred_id, registered_player, "yes", is_admin=True
        )
        assert result["can_resolve"]
        assert result["is_admin"]

    def test_cancel_by_admin_skips_internal_check(self, prediction_service):
        """Test that cancel_by_admin doesn't check internal admin list."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Non-admin user (123) should succeed with cancel_by_admin
        result = prediction_service.cancel_by_admin(pred_id, 123)
        assert result["prediction_id"] == pred_id
        assert result["cancelled_by"] == 123

    def test_check_and_lock_expired(self, prediction_service):
        """Test that check_and_lock_expired only locks expired predictions."""
        closes_at_future = int(time.time()) + 3600
        closes_at_past = int(time.time()) - 100

        pred1 = prediction_service.create_prediction(1, 999, "Future close?", closes_at_future)
        pred2 = prediction_service.create_prediction(1, 999, "Past close?", closes_at_future)

        # Manually set pred2 to past close time
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (closes_at_past, pred2["prediction_id"]),
            )

        # Run lock check
        locked = prediction_service.check_and_lock_expired(1)

        # Only pred2 should be locked
        assert pred2["prediction_id"] in locked
        assert pred1["prediction_id"] not in locked

        # Verify statuses
        assert prediction_service.get_prediction(pred1["prediction_id"])["status"] == "open"
        assert prediction_service.get_prediction(pred2["prediction_id"])["status"] == "locked"

    def test_odds_with_zero_pools(self, prediction_service):
        """Test odds calculation with no bets."""
        odds = prediction_service.calculate_odds(0, 0)
        assert odds["yes"] == 0.0
        assert odds["no"] == 0.0

    def test_odds_with_one_sided_bets(self, prediction_service):
        """Test odds when all bets on one side."""
        # All bets on YES - NO has infinite theoretical odds
        odds = prediction_service.calculate_odds(100, 0)
        assert odds["yes"] == 1.0  # 100/100
        assert odds["no"] == 0.0  # No NO pool

        # All bets on NO
        odds = prediction_service.calculate_odds(0, 100)
        assert odds["yes"] == 0.0
        assert odds["no"] == 1.0

    def test_odds_with_asymmetric_pools(self, prediction_service):
        """Test odds with heavily skewed pools."""
        odds = prediction_service.calculate_odds(10, 90)
        assert odds["yes"] == 10.0  # 100/10
        assert odds["no"] == pytest.approx(1.11, rel=0.01)  # 100/90

    def test_update_discord_ids(self, prediction_service):
        """Test updating Discord message/thread IDs."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at)
        pred_id = pred["prediction_id"]

        # Update thread and message IDs
        prediction_service.update_discord_ids(
            prediction_id=pred_id,
            thread_id=111222333,
            embed_message_id=444555666,
        )

        # Verify they're stored
        updated = prediction_service.get_prediction(pred_id)
        assert updated["thread_id"] == 111222333
        assert updated["embed_message_id"] == 444555666

    def test_get_resolution_votes(self, prediction_service, registered_player, registered_player2):
        """Test getting resolution vote counts."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Set to locked and past close time
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 1, pred_id),
            )

        # Add some votes
        prediction_service.add_resolution_vote(pred_id, registered_player, "yes")
        prediction_service.add_resolution_vote(pred_id, registered_player2, "no")

        votes = prediction_service.get_resolution_votes(pred_id)
        assert votes["yes"] == 1
        assert votes["no"] == 1

    def test_can_resolve_checks_threshold(self, prediction_service, registered_player, registered_player2, registered_player3):
        """Test can_resolve method with specific outcome."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Set to locked and past close time
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 1, pred_id),
            )

        # No votes yet
        assert not prediction_service.can_resolve(pred_id)
        assert not prediction_service.can_resolve(pred_id, "yes")

        # Add 2 YES votes - not enough
        prediction_service.add_resolution_vote(pred_id, registered_player, "yes")
        prediction_service.add_resolution_vote(pred_id, registered_player2, "yes")
        assert not prediction_service.can_resolve(pred_id, "yes")

        # Add 3rd YES vote - enough
        prediction_service.add_resolution_vote(pred_id, registered_player3, "yes")
        assert prediction_service.can_resolve(pred_id, "yes")
        assert prediction_service.can_resolve(pred_id)  # Any outcome

    def test_get_pending_outcome(self, prediction_service, registered_player, registered_player2, registered_player3):
        """Test getting pending outcome when threshold reached."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Set to locked and past close time
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 1, pred_id),
            )

        # No pending outcome yet
        assert prediction_service.get_pending_outcome(pred_id) is None

        # Add 3 NO votes
        prediction_service.add_resolution_vote(pred_id, registered_player, "no")
        prediction_service.add_resolution_vote(pred_id, registered_player2, "no")
        prediction_service.add_resolution_vote(pred_id, registered_player3, "no")

        assert prediction_service.get_pending_outcome(pred_id) == "no"

    def test_get_prediction_enriches_with_odds(self, prediction_service, registered_player, registered_player2):
        """Test that get_prediction includes odds data."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Place some bets
        prediction_service.place_bet(pred_id, registered_player, "yes", 30)
        prediction_service.place_bet(pred_id, registered_player2, "no", 70)

        enriched = prediction_service.get_prediction(pred_id)
        assert enriched["yes_total"] == 30
        assert enriched["no_total"] == 70
        assert enriched["total_pool"] == 100
        assert "odds" in enriched
        assert enriched["odds"]["yes"] == pytest.approx(3.33, rel=0.01)
        assert enriched["odds"]["no"] == pytest.approx(1.43, rel=0.01)

    def test_question_whitespace_trimmed(self, prediction_service):
        """Test that question whitespace is trimmed."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(
            1, 999, "   Will it work?   ", closes_at
        )
        assert pred["question"] == "Will it work?"

    def test_vote_on_resolved_prediction_rejected(self, prediction_service, registered_player):
        """Test that voting on resolved prediction fails."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Resolve it
        prediction_service.prediction_repo.resolve_prediction(pred_id, "yes", 999)

        with pytest.raises(ValueError, match="already been resolved"):
            prediction_service.add_resolution_vote(pred_id, registered_player, "no")

    def test_vote_on_cancelled_prediction_rejected(self, prediction_service, registered_player):
        """Test that voting on cancelled prediction fails."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Cancel it
        prediction_service.cancel(pred_id, 999)

        with pytest.raises(ValueError, match="cancelled"):
            prediction_service.add_resolution_vote(pred_id, registered_player, "no")

    def test_get_user_position_via_service(self, prediction_service, registered_player):
        """Test get_user_position service method."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # No position initially
        assert prediction_service.get_user_position(pred_id, registered_player) is None

        # Place bet
        prediction_service.place_bet(pred_id, registered_player, "yes", 25)

        position = prediction_service.get_user_position(pred_id, registered_player)
        assert position is not None
        assert position["position"] == "yes"
        assert position["total_amount"] == 25

    def test_get_user_active_positions_via_service(self, prediction_service, registered_player):
        """Test get_user_active_positions service method."""
        closes_at = int(time.time()) + 3600
        pred1 = prediction_service.create_prediction(1, 999, "Question one?", closes_at)
        pred2 = prediction_service.create_prediction(1, 999, "Question two?", closes_at)

        prediction_service.place_bet(pred1["prediction_id"], registered_player, "yes", 10)
        prediction_service.place_bet(pred2["prediction_id"], registered_player, "no", 20)

        positions = prediction_service.get_user_active_positions(registered_player)
        assert len(positions) == 2

    def test_large_pool_integer_rounding(self, prediction_repo, player_repo):
        """Test that large pools with odd division handle rounding."""
        # Create 3 players
        for i in range(1, 4):
            player_repo.add(discord_id=i, discord_username=f"P{i}", initial_mmr=3000)
            player_repo.update_balance(i, 1000)

        closes_at = int(time.time()) + 3600
        pred_id = prediction_repo.create_prediction(1, 999, "Test?", closes_at)

        # 3 winners betting unequal amounts that don't divide evenly
        prediction_repo.place_bet_atomic(pred_id, 1, "yes", 33)  # 33/100 = 33%
        prediction_repo.place_bet_atomic(pred_id, 2, "yes", 33)  # 33/100 = 33%
        prediction_repo.place_bet_atomic(pred_id, 3, "yes", 34)  # 34/100 = 34%

        # Loser
        player_repo.add(discord_id=4, discord_username="P4", initial_mmr=3000)
        player_repo.update_balance(4, 1000)
        prediction_repo.place_bet_atomic(pred_id, 4, "no", 100)

        # Total pool = 200, winner pool = 100
        result = prediction_repo.settle_prediction_bets(pred_id, "yes")

        # Check all payouts sum to 200
        total_payout = sum(w["payout"] for w in result["winners"])
        assert total_payout == 200

    def test_change_vote_rejected(self, prediction_service, registered_player):
        """Test that changing vote to different outcome fails."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at)
        pred_id = pred["prediction_id"]

        # Set to locked and past close time
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 1, pred_id),
            )

        # First vote YES
        prediction_service.add_resolution_vote(pred_id, registered_player, "yes")

        # Try to change to NO - should fail
        with pytest.raises(ValueError, match="different outcome"):
            prediction_service.add_resolution_vote(pred_id, registered_player, "no")

    def test_same_vote_twice_is_noop(self, prediction_service, registered_player):
        """Test that voting same outcome twice is allowed (no-op)."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at)
        pred_id = pred["prediction_id"]

        # Set to locked and past close time
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")
        with prediction_service.prediction_repo.connection() as conn:
            conn.execute(
                "UPDATE predictions SET closes_at = ? WHERE prediction_id = ?",
                (int(time.time()) - 1, pred_id),
            )

        # Vote YES twice - should not raise
        result1 = prediction_service.add_resolution_vote(pred_id, registered_player, "yes")
        result2 = prediction_service.add_resolution_vote(pred_id, registered_player, "yes")

        # Count should still be 1 (not 2)
        assert result1["yes_count"] == 1
        assert result2["yes_count"] == 1

    def test_resolve_cancelled_prediction_rejected(self, prediction_service):
        """Test that resolving a cancelled prediction fails."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        # Cancel it
        prediction_service.cancel(pred_id, 999)

        with pytest.raises(ValueError, match="cancelled"):
            prediction_service.resolve(pred_id, "yes", 999)

    def test_get_odds_service_method(self, prediction_service, registered_player, registered_player2):
        """Test get_odds standalone method."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        prediction_service.place_bet(pred_id, registered_player, "yes", 40)
        prediction_service.place_bet(pred_id, registered_player2, "no", 60)

        odds_info = prediction_service.get_odds(pred_id)
        assert odds_info["yes_total"] == 40
        assert odds_info["no_total"] == 60
        assert odds_info["total_pool"] == 100
        assert odds_info["odds"]["yes"] == 2.5  # 100/40
        assert odds_info["odds"]["no"] == pytest.approx(1.67, rel=0.01)

    def test_bet_returns_updated_odds(self, prediction_service, registered_player, registered_player2):
        """Test that place_bet returns updated odds."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        result1 = prediction_service.place_bet(pred_id, registered_player, "yes", 50)
        assert result1["odds"]["yes"] == 1.0  # Only bet so far

        result2 = prediction_service.place_bet(pred_id, registered_player2, "no", 50)
        assert result2["odds"]["yes"] == 2.0  # 100/50
        assert result2["odds"]["no"] == 2.0  # 100/50

    def test_can_resolve_returns_false_for_resolved(self, prediction_service):
        """Test can_resolve returns false for already resolved predictions."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test?", closes_at)
        pred_id = pred["prediction_id"]

        prediction_service.prediction_repo.resolve_prediction(pred_id, "yes", 999)

        assert not prediction_service.can_resolve(pred_id)

    def test_can_resolve_returns_false_for_nonexistent(self, prediction_service):
        """Test can_resolve returns false for non-existent prediction."""
        assert not prediction_service.can_resolve(99999)

    def test_is_admin_check(self, prediction_service):
        """Test is_admin method."""
        assert prediction_service.is_admin(999)  # Configured admin
        assert not prediction_service.is_admin(123)  # Not admin

    def test_multiple_guilds_isolation(self, prediction_service):
        """Test that predictions are isolated by guild."""
        closes_at = int(time.time()) + 3600
        pred1 = prediction_service.create_prediction(1, 999, "Guild 1 Q?", closes_at)
        pred2 = prediction_service.create_prediction(2, 999, "Guild 2 Q?", closes_at)

        guild1_active = prediction_service.get_active_predictions(1)
        guild2_active = prediction_service.get_active_predictions(2)

        assert len(guild1_active) == 1
        assert guild1_active[0]["question"] == "Guild 1 Q?"
        assert len(guild2_active) == 1
        assert guild2_active[0]["question"] == "Guild 2 Q?"

    def test_get_user_resolved_positions(self, prediction_service, registered_player, registered_player2):
        """Test getting user's resolved positions with payout info."""
        closes_at = int(time.time()) + 3600
        pred = prediction_service.create_prediction(1, 999, "Test question?", closes_at)
        pred_id = pred["prediction_id"]

        # Place bets
        prediction_service.place_bet(pred_id, registered_player, "yes", 30)
        prediction_service.place_bet(pred_id, registered_player2, "no", 70)

        # No resolved positions yet
        assert len(prediction_service.get_user_resolved_positions(registered_player)) == 0

        # Resolve the prediction
        prediction_service.prediction_repo.update_prediction_status(pred_id, "locked")
        prediction_service.resolve(pred_id, "yes", 999)

        # Now should have resolved position
        positions = prediction_service.get_user_resolved_positions(registered_player)
        assert len(positions) == 1
        assert positions[0]["prediction_id"] == pred_id
        assert positions[0]["position"] == "yes"
        assert positions[0]["outcome"] == "yes"
        assert positions[0]["total_amount"] == 30
        assert positions[0]["payout"] == 100  # Won the entire pool

        # Loser should also have a resolved position
        loser_positions = prediction_service.get_user_resolved_positions(registered_player2)
        assert len(loser_positions) == 1
        assert loser_positions[0]["position"] == "no"
        assert loser_positions[0]["outcome"] == "yes"
        assert loser_positions[0]["payout"] is None  # Lost
