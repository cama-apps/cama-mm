"""Tests for dig splash events (collateral JC burns)."""

from __future__ import annotations

import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_service import DigService
from services.dig_splash import (
    ACTIVE_DIGGERS_LOOKBACK_DAYS,
    SplashResult,
    resolve_splash,
)
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repo, discord_id: int, balance: int = 100) -> None:
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"User{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)
    # Stamp last_match_date so the lottery pool picks them up.
    import datetime
    with player_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE players SET last_match_date = ? WHERE discord_id = ? AND guild_id = ?",
            (datetime.datetime.now(datetime.UTC).isoformat(), discord_id, TEST_GUILD_ID),
        )


class TestResolveSplashPools:
    """Each victim-pool strategy should exclude the digger and handle empty pools cleanly."""

    def test_random_active_excludes_digger(self, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=100)
        _register(player_repository, 10002, balance=100)
        _register(player_repository, 10003, balance=100)
        # Seed RNG so sampling is deterministic.
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Test Event",
            strategy="random_active",
            victim_count=2,
            penalty_jc=5,
        )
        victim_ids = {vid for vid, _ in result.victims}
        assert 10001 not in victim_ids
        assert len(victim_ids) == 2

    def test_richest_n_excludes_digger_and_sorts(self, dig_repo, player_repository):
        _register(player_repository, 10001, balance=1000)   # digger, richest
        _register(player_repository, 10002, balance=500)
        _register(player_repository, 10003, balance=800)

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Test Event",
            strategy="richest_n",
            victim_count=2,
            penalty_jc=10,
        )
        victim_ids = [vid for vid, _ in result.victims]
        # Digger excluded; remaining ordered by balance desc => 10003 (800), 10002 (500).
        assert victim_ids == [10003, 10002]

    def test_active_diggers_only_includes_recent_diggers(self, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=100)
        _register(player_repository, 10002, balance=100)
        _register(player_repository, 10003, balance=100)
        # Only 10002 has logged a dig action recently; 10003 hasn't dug.
        dig_repo.log_action(
            actor_id=10002, guild_id=TEST_GUILD_ID, action_type="dig", detail={},
        )
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Test Event",
            strategy="active_diggers",
            victim_count=3,
            penalty_jc=5,
        )
        victim_ids = {vid for vid, _ in result.victims}
        assert victim_ids == {10002}

    def test_empty_pool_returns_empty_result(self, dig_repo, player_repository):
        _register(player_repository, 10001, balance=100)
        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Test Event",
            strategy="random_active",
            victim_count=3,
            penalty_jc=5,
        )
        assert result.victims == []
        assert result.total_burned == 0

    def test_unknown_strategy_returns_empty_result(self, dig_repo, player_repository):
        _register(player_repository, 10001, balance=100)
        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Test Event",
            strategy="not_a_real_strategy",
            victim_count=3,
            penalty_jc=5,
        )
        assert isinstance(result, SplashResult)
        assert result.victims == []


class TestResolveSplashBurns:
    """Splash burns JC: total balance loss = sum of penalties, not transferred anywhere."""

    def test_burns_exactly_penalty_per_victim(self, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=500)   # digger
        _register(player_repository, 10002, balance=500)
        _register(player_repository, 10003, balance=500)
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        # Snapshot total balance before
        total_before = (
            player_repository.get_balance(10001, TEST_GUILD_ID)
            + player_repository.get_balance(10002, TEST_GUILD_ID)
            + player_repository.get_balance(10003, TEST_GUILD_ID)
        )

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Burn Test",
            strategy="random_active",
            victim_count=2,
            penalty_jc=20,
        )
        assert result.total_burned == 40
        total_after = (
            player_repository.get_balance(10001, TEST_GUILD_ID)
            + player_repository.get_balance(10002, TEST_GUILD_ID)
            + player_repository.get_balance(10003, TEST_GUILD_ID)
        )
        # The 40 JC is burned — not transferred to the digger or anyone else.
        assert total_after == total_before - 40
        # Digger balance should be untouched by the splash itself.
        assert player_repository.get_balance(10001, TEST_GUILD_ID) == 500

    def test_clamps_penalty_to_victim_balance(self, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=500)
        _register(player_repository, 10002, balance=3)   # can only lose 3 JC
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Clamp Test",
            strategy="random_active",
            victim_count=1,
            penalty_jc=100,
        )
        assert result.victims == [(10002, 3)]
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == 0

    def test_skips_debtors(self, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=500)
        _register(player_repository, 10002, balance=-50)   # debtor — not further burned
        _register(player_repository, 10003, balance=100)
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Debtor Test",
            strategy="random_active",
            victim_count=2,
            penalty_jc=10,
        )
        # Sample is first-two order: 10002 (debtor, skipped), 10003 (burned 10).
        victim_ids = {vid for vid, _ in result.victims}
        assert 10002 not in victim_ids
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == -50


class TestActiveDiggersLookback:
    """Verify the ACTIVE_DIGGERS_LOOKBACK_DAYS constant is respected."""

    def test_lookback_excludes_old_digs(self, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=100)
        _register(player_repository, 10002, balance=100)

        # Log a dig older than the lookback window by rewriting created_at.
        dig_repo.log_action(
            actor_id=10002, guild_id=TEST_GUILD_ID, action_type="dig", detail={},
        )
        old_time = int(time.time()) - (ACTIVE_DIGGERS_LOOKBACK_DAYS + 1) * 86400
        with dig_repo.connection() as conn:
            conn.cursor().execute(
                "UPDATE dig_actions SET created_at = ? WHERE actor_id = ? AND guild_id = ?",
                (old_time, 10002, TEST_GUILD_ID),
            )

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Stale Dig",
            strategy="active_diggers",
            victim_count=3,
            penalty_jc=5,
        )
        assert result.victims == []
