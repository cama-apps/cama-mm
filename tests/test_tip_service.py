"""Tests for TipService."""

from repositories.tip_repository import TipRepository
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


def test_get_total_fees_collected_none_guild_same_as_zero(repo_db_path):
    repo = TipRepository(repo_db_path)
    service = TipService(repo)

    service.log_tip(1, 2, amount=10, fee=2, guild_id=None)
    assert service.get_total_fees_collected(None) == service.get_total_fees_collected(0) == 2
