"""Tests for TipService and the tip money path."""

from repositories.tip_repository import TipRepository
from services.player_service import PlayerService
from services.tip_service import TipService
from tests.conftest import TEST_GUILD_ID


def test_log_tip_delegates_to_repository(repo_db_path):
    repo = TipRepository(repo_db_path)
    service = TipService(repo)

    tx_id = service.log_tip(
        sender_id=1001,
        recipient_id=1002,
        amount=50,
        fee=5,
        guild_id=TEST_GUILD_ID,
    )
    assert isinstance(tx_id, int)

    tips = service.get_tips_by_sender(1001, guild_id=TEST_GUILD_ID, limit=5)
    assert len(tips) == 1
    assert tips[0]["amount"] == 50
    assert tips[0]["fee"] == 5


def test_tip_atomic_debits_sender_and_credits_recipient_exactly(player_repository):
    """The real /tip money path (``PlayerService.tip_atomic``, the call the
    tip command makes): sender pays amount + fee, recipient receives exactly
    amount — the fee goes to the nonprofit fund, never to the recipient."""
    service = PlayerService(player_repository)
    for did in (1001, 1002):
        player_repository.add(
            discord_id=did,
            discord_username=f"Player{did}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    player_repository.update_balance(1001, TEST_GUILD_ID, 500)
    sender_before = player_repository.get_balance(1001, TEST_GUILD_ID)
    recipient_before = player_repository.get_balance(1002, TEST_GUILD_ID)

    result = service.tip_atomic(1001, 1002, TEST_GUILD_ID, amount=50, fee=5)

    assert player_repository.get_balance(1001, TEST_GUILD_ID) == sender_before - 55
    assert player_repository.get_balance(1002, TEST_GUILD_ID) == recipient_before + 50
    assert result["from_new_balance"] == sender_before - 55
    assert result["to_new_balance"] == recipient_before + 50


def test_get_total_fees_collected_none_guild_same_as_zero(repo_db_path):
    repo = TipRepository(repo_db_path)
    service = TipService(repo)

    service.log_tip(1, 2, amount=10, fee=2, guild_id=None)
    assert service.get_total_fees_collected(None) == service.get_total_fees_collected(0) == 2
