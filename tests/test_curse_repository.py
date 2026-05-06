"""Tests for CurseRepository: cast/extend, count, multi-row, expiry filter."""

from __future__ import annotations

import time

import pytest

from repositories.curse_repository import CurseRepository
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def curse_repo(repo_db_path):
    return CurseRepository(repo_db_path)


class TestCurseRepository:
    def test_cast_inserts_new_curse(self, curse_repo):
        before = int(time.time())
        expires_at = curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=100, target_id=200, days=7
        )
        # 7 days = 604800 seconds; allow ±5s for clock drift
        delta = expires_at - before
        assert 7 * 86400 - 5 <= delta <= 7 * 86400 + 5

        count = curse_repo.count_active_curses_for_target(
            target_id=200, guild_id=TEST_GUILD_ID, now=int(time.time())
        )
        assert count == 1

    def test_same_caster_extends_existing_curse(self, curse_repo):
        first_expiry = curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=100, target_id=200, days=7
        )
        second_expiry = curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=100, target_id=200, days=7
        )
        # Second cast adds 7 days to existing expiry, not now+7d
        assert second_expiry == first_expiry + 7 * 86400

        # Still only one row for this caster→target pair
        count = curse_repo.count_active_curses_for_target(
            target_id=200, guild_id=TEST_GUILD_ID, now=int(time.time())
        )
        assert count == 1

    def test_different_casters_create_separate_rows(self, curse_repo):
        curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=100, target_id=200, days=7
        )
        curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=101, target_id=200, days=7
        )
        curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=102, target_id=200, days=7
        )
        count = curse_repo.count_active_curses_for_target(
            target_id=200, guild_id=TEST_GUILD_ID, now=int(time.time())
        )
        assert count == 3

    def test_count_respects_expiry(self, curse_repo):
        # Cast a curse that expires in 7 days
        curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=100, target_id=200, days=7
        )
        # Query "now" set to 8 days in the future — curse should be expired
        future = int(time.time()) + 8 * 86400
        count = curse_repo.count_active_curses_for_target(
            target_id=200, guild_id=TEST_GUILD_ID, now=future
        )
        assert count == 0

    def test_count_respects_target_isolation(self, curse_repo):
        curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=100, target_id=200, days=7
        )
        # Different target — should be 0
        count = curse_repo.count_active_curses_for_target(
            target_id=999, guild_id=TEST_GUILD_ID, now=int(time.time())
        )
        assert count == 0

    def test_count_respects_guild_isolation(self, curse_repo):
        curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=100, target_id=200, days=7
        )
        # Different guild — should be 0
        count = curse_repo.count_active_curses_for_target(
            target_id=200, guild_id=99999, now=int(time.time())
        )
        assert count == 0

    def test_none_guild_normalizes_to_zero(self, curse_repo):
        # Cast with guild_id=None
        curse_repo.cast_or_extend(
            guild_id=None, caster_id=100, target_id=200, days=7
        )
        # Query with guild_id=None and 0 should both find it
        count_none = curse_repo.count_active_curses_for_target(
            target_id=200, guild_id=None, now=int(time.time())
        )
        count_zero = curse_repo.count_active_curses_for_target(
            target_id=200, guild_id=0, now=int(time.time())
        )
        assert count_none == 1
        assert count_zero == 1

    def test_self_curse_allowed(self, curse_repo):
        # The repository doesn't restrict self-curse — that's a service-level concern
        expires = curse_repo.cast_or_extend(
            guild_id=TEST_GUILD_ID, caster_id=100, target_id=100, days=7
        )
        assert expires > int(time.time())
        count = curse_repo.count_active_curses_for_target(
            target_id=100, guild_id=TEST_GUILD_ID, now=int(time.time())
        )
        assert count == 1
