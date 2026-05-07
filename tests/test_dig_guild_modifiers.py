"""Unit tests for the DigGuildModifierRepository (helltide-bell support)."""

from __future__ import annotations

import time

import pytest

from repositories.dig_guild_modifier_repository import DigGuildModifierRepository


@pytest.fixture
def gm_repo(repo_db_path):
    return DigGuildModifierRepository(repo_db_path)


class TestSetAndQuery:
    def test_set_then_active(self, gm_repo, guild_id):
        expires = gm_repo.set_modifier(guild_id, "helltide_active", duration_seconds=600, payload={"tax_per_dig": 5})
        now = int(time.time())
        assert expires > now
        assert gm_repo.is_active(guild_id, "helltide_active") is True

    def test_inactive_when_unset(self, gm_repo, guild_id):
        assert gm_repo.is_active(guild_id, "no_such_mod") is False

    def test_get_active_returns_payload(self, gm_repo, guild_id):
        gm_repo.set_modifier(guild_id, "helltide_active", duration_seconds=600, payload={"tax_per_dig": 7})
        rows = gm_repo.get_active(guild_id)
        assert len(rows) == 1
        row = rows[0]
        assert row["modifier_id"] == "helltide_active"
        assert row["payload"]["tax_per_dig"] == 7

    def test_re_set_extends_expiry(self, gm_repo, guild_id):
        a = gm_repo.set_modifier(guild_id, "helltide_active", duration_seconds=300)
        b = gm_repo.set_modifier(guild_id, "helltide_active", duration_seconds=300)
        assert b > a


class TestExpiry:
    def test_get_active_filters_expired(self, gm_repo, guild_id):
        gm_repo.set_modifier(guild_id, "stale", duration_seconds=600)
        # Query at a time far in the future.
        rows = gm_repo.get_active(guild_id, now=2_000_000_000)
        assert rows == []

    def test_clear_expired_drops_rows(self, gm_repo, guild_id):
        gm_repo.set_modifier(guild_id, "stale", duration_seconds=600)
        removed = gm_repo.clear_expired(now=2_000_000_000)
        assert removed == 1
        # Subsequent query returns nothing.
        rows = gm_repo.get_active(guild_id, now=2_000_000_000)
        assert rows == []


class TestGuildIsolation:
    def test_modifier_scoped_to_guild(self, gm_repo, guild_id, secondary_guild_id):
        gm_repo.set_modifier(guild_id, "helltide_active", duration_seconds=600)
        assert gm_repo.is_active(guild_id, "helltide_active") is True
        assert gm_repo.is_active(secondary_guild_id, "helltide_active") is False
