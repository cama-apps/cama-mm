"""Integration tests for RD decay with real database operations.

These tests verify:
1. last_match_date is updated when recording a match
2. RD decay is correctly applied based on last_match_date
3. The full flow: old match → load player → decay applied → new match → no decay
"""

import math
import tempfile
from datetime import datetime, timedelta, timezone

import pytest

from database import Database
from rating_system import CamaRatingSystem
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService

TEST_GUILD_ID = 12345


@pytest.fixture
def test_db():
    """Create a fresh database for each test."""
    tmp = tempfile.NamedTemporaryFile(delete=False, suffix=".db")
    tmp.close()
    db = Database(tmp.name)
    yield db
    # Database uses context managers internally, no explicit close needed


def test_last_match_date_updated_after_record(test_db):
    """Verify last_match_date is set when a match is recorded."""
    player_repo = PlayerRepository(test_db.db_path)
    match_repo = MatchRepository(test_db.db_path)
    match_service = MatchService(player_repo, match_repo, use_glicko=True)

    # Create 10 players using player_repo.add() with guild_id
    for i in range(10):
        pid = 100 + i
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
        )

    player_ids = list(range(100, 110))

    # Check last_match_date is initially NULL or created_at
    before_dates = player_repo.get_last_match_date(100, TEST_GUILD_ID)
    initial_last_match = before_dates[0] if before_dates else None

    # Shuffle and record match
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    assert result["winning_team"] == "radiant"

    # Verify last_match_date was updated for all participants
    for pid in player_ids:
        dates = player_repo.get_last_match_date(pid, TEST_GUILD_ID)
        assert dates is not None, f"Player {pid} should have dates tuple"
        last_match, created_at = dates
        assert last_match is not None, f"Player {pid} should have last_match_date set"
        # Verify it's recent (within last minute)
        last_match_dt = datetime.fromisoformat(last_match)
        if last_match_dt.tzinfo is None:
            last_match_dt = last_match_dt.replace(tzinfo=timezone.utc)
        now = datetime.now(timezone.utc)
        assert (now - last_match_dt).total_seconds() < 60, "last_match_date should be recent"


def test_rd_decay_not_applied_when_match_just_recorded(test_db):
    """After recording a match, loading the player should NOT apply decay."""
    player_repo = PlayerRepository(test_db.db_path)
    match_repo = MatchRepository(test_db.db_path)
    match_service = MatchService(player_repo, match_repo, use_glicko=True)

    # Create 10 players with known RD
    start_rd = 150.0
    for i in range(10):
        pid = 200 + i
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=start_rd,
            glicko_volatility=0.06,
        )

    player_ids = list(range(200, 210))

    # Record a match first
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    match_service.record_match("dire", guild_id=TEST_GUILD_ID)

    # Now load a player - RD should be close to what Glicko-2 set it to (decreased from match)
    # NOT increased from decay since match was just recorded
    player, _ = match_service._load_glicko_player(200, TEST_GUILD_ID)

    # After a match, RD typically decreases. It should definitely not be > start_rd
    # (which would indicate improper decay was applied)
    assert player.rd <= start_rd, f"RD should not increase after a match (got {player.rd})"


def test_rd_decay_applied_for_inactive_player(test_db):
    """Verify RD decay is applied when loading a player with old last_match_date."""
    player_repo = PlayerRepository(test_db.db_path)
    match_repo = MatchRepository(test_db.db_path)
    match_service = MatchService(player_repo, match_repo, use_glicko=True)

    # Create a player
    pid = 300
    start_rd = 100.0
    player_repo.add(
        discord_id=pid,
        discord_username="InactivePlayer",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=start_rd,
        glicko_volatility=0.06,
    )

    # Manually set last_match_date to 4 weeks ago (beyond grace period)
    four_weeks_ago = (datetime.now(timezone.utc) - timedelta(weeks=4)).isoformat()
    player_repo.update_last_match_date(pid, TEST_GUILD_ID, four_weeks_ago)

    # Load the player - RD should have decayed
    player, _ = match_service._load_glicko_player(pid, TEST_GUILD_ID)

    # Expected: sqrt(100^2 + 50^2 * 4) = sqrt(10000 + 10000) = sqrt(20000) ≈ 141.4
    expected_weeks = 4
    expected_rd = math.sqrt(start_rd * start_rd + (50.0 * 50.0) * expected_weeks)

    assert math.isclose(player.rd, expected_rd, rel_tol=0.01), \
        f"RD should decay from {start_rd} to ~{expected_rd}, got {player.rd}"


def test_rd_decay_respects_grace_period(test_db):
    """Verify RD decay is NOT applied within the grace period."""
    player_repo = PlayerRepository(test_db.db_path)
    match_repo = MatchRepository(test_db.db_path)
    match_service = MatchService(player_repo, match_repo, use_glicko=True)

    # Create a player
    pid = 400
    start_rd = 100.0
    player_repo.add(
        discord_id=pid,
        discord_username="RecentPlayer",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=start_rd,
        glicko_volatility=0.06,
    )

    # Set last_match_date to 1 week ago (within 2-week grace period)
    one_week_ago = (datetime.now(timezone.utc) - timedelta(weeks=1)).isoformat()
    player_repo.update_last_match_date(pid, TEST_GUILD_ID, one_week_ago)

    # Load the player - RD should NOT have decayed
    player, _ = match_service._load_glicko_player(pid, TEST_GUILD_ID)

    assert player.rd == start_rd, f"RD should not decay within grace period (got {player.rd})"


def test_bulk_update_and_last_match_date_are_both_applied(test_db):
    """Verify both rating updates AND last_match_date are saved after match."""
    player_repo = PlayerRepository(test_db.db_path)
    match_repo = MatchRepository(test_db.db_path)
    match_service = MatchService(player_repo, match_repo, use_glicko=True)

    # Create 10 players
    for i in range(10):
        pid = 500 + i
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,  # High RD
            glicko_volatility=0.06,
        )

    player_ids = list(range(500, 510))

    # Get initial ratings
    initial_ratings = {}
    for pid in player_ids:
        initial_ratings[pid] = player_repo.get_glicko_rating(pid, TEST_GUILD_ID)

    # Record match
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    # Verify both rating and last_match_date were updated
    for pid in player_ids:
        new_rating = player_repo.get_glicko_rating(pid, TEST_GUILD_ID)
        dates = player_repo.get_last_match_date(pid, TEST_GUILD_ID)

        # Rating should have changed (RD decreases after a match)
        assert new_rating[1] < initial_ratings[pid][1], \
            f"Player {pid} RD should decrease after match"

        # last_match_date should be set
        assert dates[0] is not None, f"Player {pid} should have last_match_date"
