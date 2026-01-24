"""
Tests for TipRepository and tip_atomic() in PlayerRepository.
"""

import pytest

from repositories.player_repository import PlayerRepository
from repositories.tip_repository import TipRepository


@pytest.fixture
def tip_repo(repo_db_path):
    """Create a TipRepository instance."""
    return TipRepository(repo_db_path)


@pytest.fixture
def player_repo(repo_db_path):
    """Create a PlayerRepository instance."""
    return PlayerRepository(repo_db_path)


def register_player(player_repo, discord_id, balance=100):
    """Helper to register a test player with a given balance."""
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        initial_mmr=3000,
    )
    # Set initial balance
    if balance != 3:  # Default balance is 3
        player_repo.add_balance(discord_id, balance - 3)


class TestTipRepository:
    """Tests for TipRepository."""

    def test_log_tip_creates_record(self, tip_repo, player_repo):
        """Test that log_tip creates a transaction record."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)

        tip_id = tip_repo.log_tip(
            sender_id=1,
            recipient_id=2,
            amount=10,
            fee=1,
            guild_id=12345,
        )

        assert tip_id is not None
        assert tip_id > 0

    def test_log_tip_stores_correct_values(self, tip_repo, player_repo):
        """Test that log_tip stores the correct values."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)

        tip_repo.log_tip(
            sender_id=1,
            recipient_id=2,
            amount=50,
            fee=5,
            guild_id=99999,
        )

        # Retrieve the tip
        tips = tip_repo.get_tips_by_sender(1)
        assert len(tips) == 1
        tip = tips[0]
        assert tip["sender_id"] == 1
        assert tip["recipient_id"] == 2
        assert tip["amount"] == 50
        assert tip["fee"] == 5
        assert tip["guild_id"] == 99999
        assert tip["timestamp"] > 0

    def test_log_tip_null_guild_id_normalized_to_zero(self, tip_repo, player_repo):
        """Test that None guild_id is normalized to 0."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)

        tip_repo.log_tip(
            sender_id=1,
            recipient_id=2,
            amount=10,
            fee=1,
            guild_id=None,
        )

        tips = tip_repo.get_tips_by_sender(1)
        assert tips[0]["guild_id"] == 0

    def test_get_tips_by_sender_returns_multiple(self, tip_repo, player_repo):
        """Test getting multiple tips sent by a user."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)
        register_player(player_repo, 3)

        tip_repo.log_tip(1, 2, 10, 1, None)
        tip_repo.log_tip(1, 3, 20, 2, None)
        tip_repo.log_tip(1, 2, 30, 3, None)

        tips = tip_repo.get_tips_by_sender(1)
        assert len(tips) == 3

    def test_get_tips_by_sender_respects_limit(self, tip_repo, player_repo):
        """Test that limit parameter works."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)

        for i in range(10):
            tip_repo.log_tip(1, 2, 10 + i, 1, None)

        tips = tip_repo.get_tips_by_sender(1, limit=3)
        assert len(tips) == 3

    def test_get_tips_by_sender_ordered_by_timestamp_desc(self, tip_repo, player_repo):
        """Test that tips are ordered newest first (by timestamp, then by id)."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)

        tip_repo.log_tip(1, 2, 10, 1, None)
        tip_repo.log_tip(1, 2, 20, 2, None)
        tip_repo.log_tip(1, 2, 30, 3, None)

        tips = tip_repo.get_tips_by_sender(1)
        # All three tips should be present
        assert len(tips) == 3
        amounts = {tip["amount"] for tip in tips}
        assert amounts == {10, 20, 30}
        # Timestamps should be in descending order (or equal)
        for i in range(len(tips) - 1):
            assert tips[i]["timestamp"] >= tips[i + 1]["timestamp"]

    def test_get_tips_by_recipient_returns_multiple(self, tip_repo, player_repo):
        """Test getting multiple tips received by a user."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)
        register_player(player_repo, 3)

        tip_repo.log_tip(1, 3, 10, 1, None)
        tip_repo.log_tip(2, 3, 20, 2, None)

        tips = tip_repo.get_tips_by_recipient(3)
        assert len(tips) == 2

    def test_get_tips_by_recipient_isolates_user(self, tip_repo, player_repo):
        """Test that get_tips_by_recipient only returns tips for that user."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)
        register_player(player_repo, 3)

        tip_repo.log_tip(1, 2, 10, 1, None)
        tip_repo.log_tip(1, 3, 20, 2, None)

        tips = tip_repo.get_tips_by_recipient(2)
        assert len(tips) == 1
        assert tips[0]["recipient_id"] == 2

    def test_get_total_fees_collected_all_guilds(self, tip_repo, player_repo):
        """Test calculating total fees across all guilds."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)

        tip_repo.log_tip(1, 2, 100, 5, 111)
        tip_repo.log_tip(1, 2, 200, 10, 222)
        tip_repo.log_tip(1, 2, 50, 3, None)

        total = tip_repo.get_total_fees_collected()
        assert total == 18  # 5 + 10 + 3

    def test_get_total_fees_collected_specific_guild(self, tip_repo, player_repo):
        """Test calculating total fees for a specific guild."""
        register_player(player_repo, 1)
        register_player(player_repo, 2)

        tip_repo.log_tip(1, 2, 100, 5, 111)
        tip_repo.log_tip(1, 2, 200, 10, 222)
        tip_repo.log_tip(1, 2, 50, 3, 111)

        total = tip_repo.get_total_fees_collected(guild_id=111)
        assert total == 8  # 5 + 3

    def test_get_total_fees_collected_empty_returns_zero(self, tip_repo):
        """Test that empty table returns 0."""
        total = tip_repo.get_total_fees_collected()
        assert total == 0


class TestTipAtomic:
    """Tests for tip_atomic() in PlayerRepository."""

    def test_tip_atomic_transfers_correct_amounts(self, player_repo):
        """Test that tip_atomic transfers the correct amounts."""
        register_player(player_repo, 1, balance=100)
        register_player(player_repo, 2, balance=50)

        result = player_repo.tip_atomic(
            from_discord_id=1,
            to_discord_id=2,
            amount=20,
            fee=2,
        )

        # Sender pays amount + fee
        assert result["from_new_balance"] == 78  # 100 - 20 - 2
        # Recipient gets only amount
        assert result["to_new_balance"] == 70  # 50 + 20
        assert result["amount"] == 20
        assert result["fee"] == 2

    def test_tip_atomic_burns_fee(self, player_repo):
        """Test that the fee is burned (removed from economy)."""
        register_player(player_repo, 1, balance=100)
        register_player(player_repo, 2, balance=50)

        player_repo.tip_atomic(
            from_discord_id=1,
            to_discord_id=2,
            amount=20,
            fee=5,
        )

        # Total economy before: 150
        # Total economy after: 78 + 70 = 148 (fee of 2 burned... wait let me recalc)
        # Sender: 100 - 20 - 5 = 75
        # Recipient: 50 + 20 = 70
        # Total: 145, burned: 5
        sender = player_repo.get_by_id(1)
        recipient = player_repo.get_by_id(2)
        total_after = sender.jopacoin_balance + recipient.jopacoin_balance
        assert total_after == 145  # 150 - 5 fee

    def test_tip_atomic_insufficient_funds_raises(self, player_repo):
        """Test that insufficient funds raises ValueError."""
        register_player(player_repo, 1, balance=10)
        register_player(player_repo, 2, balance=50)

        with pytest.raises(ValueError) as exc_info:
            player_repo.tip_atomic(
                from_discord_id=1,
                to_discord_id=2,
                amount=10,
                fee=1,  # Total: 11, but only have 10
            )
        assert "Insufficient" in str(exc_info.value)

    def test_tip_atomic_sender_not_found_raises(self, player_repo):
        """Test that missing sender raises ValueError."""
        register_player(player_repo, 2, balance=50)

        with pytest.raises(ValueError) as exc_info:
            player_repo.tip_atomic(
                from_discord_id=999,  # Not registered
                to_discord_id=2,
                amount=10,
                fee=1,
            )
        assert "Sender" in str(exc_info.value) or "not found" in str(exc_info.value).lower()

    def test_tip_atomic_recipient_not_found_raises(self, player_repo):
        """Test that missing recipient raises ValueError."""
        register_player(player_repo, 1, balance=100)

        with pytest.raises(ValueError) as exc_info:
            player_repo.tip_atomic(
                from_discord_id=1,
                to_discord_id=999,  # Not registered
                amount=10,
                fee=1,
            )
        assert "Recipient" in str(exc_info.value) or "not found" in str(exc_info.value).lower()

    def test_tip_atomic_negative_amount_raises(self, player_repo):
        """Test that negative amount raises ValueError."""
        register_player(player_repo, 1, balance=100)
        register_player(player_repo, 2, balance=50)

        with pytest.raises(ValueError) as exc_info:
            player_repo.tip_atomic(
                from_discord_id=1,
                to_discord_id=2,
                amount=-10,
                fee=1,
            )
        assert "positive" in str(exc_info.value).lower()

    def test_tip_atomic_zero_amount_raises(self, player_repo):
        """Test that zero amount raises ValueError."""
        register_player(player_repo, 1, balance=100)
        register_player(player_repo, 2, balance=50)

        with pytest.raises(ValueError) as exc_info:
            player_repo.tip_atomic(
                from_discord_id=1,
                to_discord_id=2,
                amount=0,
                fee=1,
            )
        assert "positive" in str(exc_info.value).lower()

    def test_tip_atomic_negative_fee_raises(self, player_repo):
        """Test that negative fee raises ValueError."""
        register_player(player_repo, 1, balance=100)
        register_player(player_repo, 2, balance=50)

        with pytest.raises(ValueError) as exc_info:
            player_repo.tip_atomic(
                from_discord_id=1,
                to_discord_id=2,
                amount=10,
                fee=-1,
            )
        assert "negative" in str(exc_info.value).lower()

    def test_tip_atomic_zero_fee_allowed(self, player_repo):
        """Test that zero fee is allowed (no fee scenario)."""
        register_player(player_repo, 1, balance=100)
        register_player(player_repo, 2, balance=50)

        result = player_repo.tip_atomic(
            from_discord_id=1,
            to_discord_id=2,
            amount=10,
            fee=0,
        )

        assert result["from_new_balance"] == 90
        assert result["to_new_balance"] == 60
        assert result["fee"] == 0

    def test_tip_atomic_exact_balance(self, player_repo):
        """Test tip with exact balance (amount + fee = balance)."""
        register_player(player_repo, 1, balance=11)
        register_player(player_repo, 2, balance=0)

        result = player_repo.tip_atomic(
            from_discord_id=1,
            to_discord_id=2,
            amount=10,
            fee=1,
        )

        assert result["from_new_balance"] == 0
        assert result["to_new_balance"] == 10

    def test_tip_atomic_is_atomic_on_failure(self, player_repo):
        """Test that failed tip doesn't modify any balances."""
        register_player(player_repo, 1, balance=5)
        register_player(player_repo, 2, balance=50)

        original_sender_balance = player_repo.get_by_id(1).jopacoin_balance
        original_recipient_balance = player_repo.get_by_id(2).jopacoin_balance

        with pytest.raises(ValueError):
            player_repo.tip_atomic(
                from_discord_id=1,
                to_discord_id=2,
                amount=10,
                fee=1,
            )

        # Verify no changes occurred
        assert player_repo.get_by_id(1).jopacoin_balance == original_sender_balance
        assert player_repo.get_by_id(2).jopacoin_balance == original_recipient_balance

    def test_tip_atomic_large_amounts(self, player_repo):
        """Test tip with large amounts."""
        register_player(player_repo, 1, balance=1000000)
        register_player(player_repo, 2, balance=0)

        result = player_repo.tip_atomic(
            from_discord_id=1,
            to_discord_id=2,
            amount=500000,
            fee=50000,
        )

        assert result["from_new_balance"] == 450000
        assert result["to_new_balance"] == 500000

    def test_tip_atomic_tracks_lowest_balance(self, player_repo):
        """Test that lowest_balance_ever is updated correctly in the database."""
        register_player(player_repo, 1, balance=100)
        register_player(player_repo, 2, balance=50)

        player_repo.tip_atomic(
            from_discord_id=1,
            to_discord_id=2,
            amount=90,
            fee=5,  # New balance = 5
        )

        sender = player_repo.get_by_id(1)
        assert sender.jopacoin_balance == 5

        # Verify lowest_balance_ever in DB directly (not exposed on Player model)
        with player_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT lowest_balance_ever FROM players WHERE discord_id = ?",
                (1,),
            )
            row = cursor.fetchone()
            assert row["lowest_balance_ever"] <= 5
