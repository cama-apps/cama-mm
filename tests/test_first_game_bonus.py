"""Tests for the first game of the night bonus feature."""

from datetime import UTC, datetime
from unittest.mock import patch
from zoneinfo import ZoneInfo

from config import FIRST_GAME_BONUS, FIRST_GAME_RESET_HOUR
from tests.conftest import TEST_GUILD_ID


class TestGetMatchCountSince:
    """Repository: get_match_count_since returns correct counts."""

    def test_no_matches(self, match_repository):
        count = match_repository.get_match_count_since(TEST_GUILD_ID, "2020-01-01 00:00:00")
        assert count == 0

    def test_match_before_boundary(self, match_repository):
        """Matches recorded before the boundary are not counted."""
        match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )
        # Use a future date as boundary — the match should not be counted
        count = match_repository.get_match_count_since(TEST_GUILD_ID, "2099-01-01 00:00:00")
        assert count == 0

    def test_match_after_boundary(self, match_repository):
        """Matches recorded after the boundary are counted."""
        match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )
        # Use a past date as boundary — the match should be counted
        count = match_repository.get_match_count_since(TEST_GUILD_ID, "2000-01-01 00:00:00")
        assert count == 1

    def test_multiple_matches(self, match_repository):
        """Multiple matches after boundary are all counted."""
        for _ in range(3):
            match_repository.record_match(
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
                guild_id=TEST_GUILD_ID,
            )
        count = match_repository.get_match_count_since(TEST_GUILD_ID, "2000-01-01 00:00:00")
        assert count == 3

    def test_guild_isolation(self, match_repository):
        """Matches in one guild don't affect count in another."""
        match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )
        other_guild = TEST_GUILD_ID + 1
        count = match_repository.get_match_count_since(other_guild, "2000-01-01 00:00:00")
        assert count == 0


class TestIsFirstGameOfNight:
    """MatchService: is_first_game_of_night boundary logic."""

    def test_no_matches_returns_true(self, match_service):
        """First game when no matches exist at all."""
        assert match_service.is_first_game_of_night(TEST_GUILD_ID) is True

    def test_with_existing_match_returns_false(self, match_service_with_betting, player_repository):
        """Not first game when a match has been recorded."""
        svc = match_service_with_betting
        # Register players and record a match directly via repo
        for pid in range(1, 11):
            player_repository.add(pid, f"player{pid}", TEST_GUILD_ID)
        svc.match_repo.record_match(
            team1_ids=list(range(1, 6)),
            team2_ids=list(range(6, 11)),
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )
        assert svc.is_first_game_of_night(TEST_GUILD_ID) is False

    def test_boundary_before_reset_hour(self, match_service):
        """Before reset hour → boundary is yesterday at reset hour."""
        la_tz = ZoneInfo("America/Los_Angeles")
        # Simulate 10am LA time (before 5pm)
        fake_now = datetime(2026, 3, 15, 10, 0, 0, tzinfo=la_tz)

        with patch("services.match_service.datetime") as mock_dt:
            mock_dt.now.return_value = fake_now
            mock_dt.side_effect = lambda *a, **kw: datetime(*a, **kw)
            result = match_service.is_first_game_of_night(TEST_GUILD_ID)

        assert result is True  # No matches exist

    def test_boundary_after_reset_hour(self, match_service):
        """After reset hour → boundary is today at reset hour."""
        la_tz = ZoneInfo("America/Los_Angeles")
        # Simulate 8pm LA time (after 5pm)
        fake_now = datetime(2026, 3, 15, 20, 0, 0, tzinfo=la_tz)

        with patch("services.match_service.datetime") as mock_dt:
            mock_dt.now.return_value = fake_now
            mock_dt.side_effect = lambda *a, **kw: datetime(*a, **kw)
            result = match_service.is_first_game_of_night(TEST_GUILD_ID)

        assert result is True  # No matches exist

    def test_boundary_calculation_before_reset_hour(self, match_service):
        """Verify that before reset hour, the boundary is yesterday's reset time."""
        la_tz = ZoneInfo("America/Los_Angeles")
        # 10am on March 15 → boundary should be March 14 at 5pm
        fake_now = datetime(2026, 3, 15, 10, 0, 0, tzinfo=la_tz)

        with patch("services.match_service.datetime") as mock_dt:
            mock_dt.now.return_value = fake_now
            mock_dt.side_effect = lambda *a, **kw: datetime(*a, **kw)

            # The boundary should be March 14 at 5pm LA time
            expected_boundary_la = datetime(2026, 3, 14, FIRST_GAME_RESET_HOUR, 0, 0, tzinfo=la_tz)
            expected_boundary_utc = expected_boundary_la.astimezone(UTC)
            expected_iso = expected_boundary_utc.strftime("%Y-%m-%d %H:%M:%S")

            # Patch the repo to capture the argument
            original_count = match_service.match_repo.get_match_count_since

            captured_args = []

            def capture_count(gid, since):
                captured_args.append((gid, since))
                return original_count(gid, since)

            match_service.match_repo.get_match_count_since = capture_count
            try:
                match_service.is_first_game_of_night(TEST_GUILD_ID)
            finally:
                match_service.match_repo.get_match_count_since = original_count

            assert len(captured_args) == 1
            assert captured_args[0][1] == expected_iso

    def test_boundary_calculation_after_reset_hour(self, match_service):
        """Verify that after reset hour, the boundary is today's reset time."""
        la_tz = ZoneInfo("America/Los_Angeles")
        # 8pm on March 15 → boundary should be March 15 at 5pm
        fake_now = datetime(2026, 3, 15, 20, 0, 0, tzinfo=la_tz)

        with patch("services.match_service.datetime") as mock_dt:
            mock_dt.now.return_value = fake_now
            mock_dt.side_effect = lambda *a, **kw: datetime(*a, **kw)

            expected_boundary_la = datetime(2026, 3, 15, FIRST_GAME_RESET_HOUR, 0, 0, tzinfo=la_tz)
            expected_boundary_utc = expected_boundary_la.astimezone(UTC)
            expected_iso = expected_boundary_utc.strftime("%Y-%m-%d %H:%M:%S")

            captured_args = []
            original_count = match_service.match_repo.get_match_count_since

            def capture_count(gid, since):
                captured_args.append((gid, since))
                return original_count(gid, since)

            match_service.match_repo.get_match_count_since = capture_count
            try:
                match_service.is_first_game_of_night(TEST_GUILD_ID)
            finally:
                match_service.match_repo.get_match_count_since = original_count

            assert len(captured_args) == 1
            assert captured_args[0][1] == expected_iso

    def test_boundary_on_spring_forward_day(self, match_service):
        """Boundary is correct on DST spring-forward day (March 8, 2026)."""
        la_tz = ZoneInfo("America/Los_Angeles")
        # 8pm PDT on spring-forward day
        fake_now = datetime(2026, 3, 8, 20, 0, 0, tzinfo=la_tz)

        with patch("services.match_service.datetime") as mock_dt:
            mock_dt.now.return_value = fake_now
            mock_dt.side_effect = lambda *a, **kw: datetime(*a, **kw)

            # 5pm PDT on March 8 = midnight UTC on March 9
            expected_boundary_la = datetime(2026, 3, 8, FIRST_GAME_RESET_HOUR, 0, 0, tzinfo=la_tz)
            expected_boundary_utc = expected_boundary_la.astimezone(UTC)
            expected_iso = expected_boundary_utc.strftime("%Y-%m-%d %H:%M:%S")

            captured_args = []
            original_count = match_service.match_repo.get_match_count_since

            def capture_count(gid, since):
                captured_args.append((gid, since))
                return original_count(gid, since)

            match_service.match_repo.get_match_count_since = capture_count
            try:
                match_service.is_first_game_of_night(TEST_GUILD_ID)
            finally:
                match_service.match_repo.get_match_count_since = original_count

            assert len(captured_args) == 1
            assert captured_args[0][1] == expected_iso


class TestAwardFirstGameBonus:
    """BettingService: award_first_game_bonus applies rewards with penalties."""

    def test_awards_bonus_to_all_players(self, betting_service, player_repository):
        """All players receive the first game bonus."""
        player_ids = [101, 102, 103]
        for pid in player_ids:
            player_repository.add(pid, f"player{pid}", TEST_GUILD_ID)

        initial_balances = {
            pid: player_repository.get_balance(pid, TEST_GUILD_ID)
            for pid in player_ids
        }

        result = betting_service.award_first_game_bonus(player_ids, TEST_GUILD_ID)

        for pid in player_ids:
            new_balance = player_repository.get_balance(pid, TEST_GUILD_ID)
            assert new_balance == initial_balances[pid] + FIRST_GAME_BONUS
            assert pid in result
            assert result[pid]["net"] == FIRST_GAME_BONUS

    def test_award_returns_penalty_info(self, betting_service, player_repository):
        """Award result includes garnishment and penalty fields."""
        player_repository.add(201, "penalized", TEST_GUILD_ID)
        result = betting_service.award_first_game_bonus([201], TEST_GUILD_ID)

        assert 201 in result
        assert "gross" in result[201]
        assert "garnished" in result[201]
        assert "net" in result[201]

    def test_award_empty_list(self, betting_service):
        """Awarding to no players returns empty dict."""
        result = betting_service.award_first_game_bonus([], TEST_GUILD_ID)
        assert result == {}
