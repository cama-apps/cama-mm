"""Tests for dig splash events (collateral JC burns)."""

from __future__ import annotations

import random
import time
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

from repositories.dig_repository import DigRepository
from services.dig_service import DigService
from services.dig_splash import (
    ACTIVE_DIGGERS_LOOKBACK_DAYS,
    SplashResult,
    resolve_splash,
)
from tests.conftest import TEST_GUILD_ID
from utils.economy_scaling import (
    scale_deflationary_minigame_jc_delta,
    scale_minigame_jc_delta,
)


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
        expected_burn = 2 * scale_deflationary_minigame_jc_delta(22)
        assert result.total_burned == expected_burn
        total_after = (
            player_repository.get_balance(10001, TEST_GUILD_ID)
            + player_repository.get_balance(10002, TEST_GUILD_ID)
            + player_repository.get_balance(10003, TEST_GUILD_ID)
        )
        # The JC is burned — not transferred to the digger or anyone else.
        assert total_after == total_before - expected_burn
        # Digger balance should be untouched by the splash itself.
        assert player_repository.get_balance(10001, TEST_GUILD_ID) == 500

    def test_skips_players_below_auto_blind_threshold(self, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=500)
        _register(player_repository, 10002, balance=3)   # below AOE threshold
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
        assert result.victims == []
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == 3

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

    def test_burn_routes_through_white_protection_gateway(
        self, dig_repo, player_repository, monkeypatch
    ):
        _register(player_repository, 10001, balance=500)
        _register(player_repository, 10002, balance=500)
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])
        protection = MagicMock()
        protection.apply_hostile_loss.return_value = SimpleNamespace(
            attempted=18,
            absorbed=10,
            applied=6,
        )

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Shield Test",
            strategy="random_active",
            victim_count=1,
            penalty_jc=20,
            protection_service=protection,
            event_key_prefix="dig:test",
        )

        assert result.victims == [(10002, 6)]
        assert result.absorbed_total == 10
        assert result.shielded_count == 1
        call = protection.apply_hostile_loss.call_args
        assert call.args[:4] == (10002, TEST_GUILD_ID, 19, "dig_splash_burn")
        assert call.kwargs["destination"] == "burn"
        assert call.kwargs["event_key"] == "dig:test:10002"
        # The fake gateway owns settlement, so the legacy debit is not repeated.
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == 500


class TestActiveDiggersLookback:
    """Verify the ACTIVE_DIGGERS_LOOKBACK_DAYS constant is respected."""

    def test_lookback_excludes_old_digs(self, dig_repo, player_repository):
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


class TestSplashGrantMode:
    """Cooperative splash (mode='grant') credits JC to the recipient instead of burning it."""

    def test_grant_credits_recipient(self, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=100)
        _register(player_repository, 10002, balance=100)
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Wisp Tether",
            strategy="random_active",
            victim_count=1,
            penalty_jc=10,
            mode="grant",
        )
        assert result.mode == "grant"
        assert len(result.victims) == 1
        vid, amount = result.victims[0]
        assert vid == 10002
        assert amount == scale_minigame_jc_delta(10)
        # Recipient balance goes UP.
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == 108

    def test_grant_ignores_zero_balance_guard(self, dig_repo, player_repository, monkeypatch):
        """Grant mode does not skip debtors — you can gift into a negative balance."""
        _register(player_repository, 10001, balance=100)
        _register(player_repository, 10002, balance=0)
        player_repository.add_balance(10002, TEST_GUILD_ID, -25)  # now -25
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Wisp Tether",
            strategy="random_active",
            victim_count=1,
            penalty_jc=10,
            mode="grant",
        )
        assert result.victims == [(10002, scale_minigame_jc_delta(10))]
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == -17


class TestSplashStealMode:
    """Steals transfer the normal scaled amount without event deflation."""

    def test_steal_does_not_strengthen_authored_amount(
        self, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10001, balance=100)
        _register(player_repository, 10002, balance=100)
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="Steal Test",
            strategy="random_active",
            victim_count=1,
            penalty_jc=10,
            mode="steal",
        )

        expected = scale_minigame_jc_delta(10)
        assert result.victims == [(10002, expected)]
        assert player_repository.get_balance(10001, TEST_GUILD_ID) == 100 + expected
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == 100 - expected


class TestResolveSplashDeepest:
    """The deepest_n strategy targets the deepest tunnels and never the digger."""

    def test_deepest_n_targets_deepest_excluding_digger(self, dig_repo, player_repository):
        # Digger plus three potential victims, all starting at balance 100.
        _register(player_repository, 10001, balance=100)  # digger — DEEPEST, must be excluded
        _register(player_repository, 10002, balance=100)
        _register(player_repository, 10003, balance=100)
        _register(player_repository, 10004, balance=100)

        # Give each a tunnel and set depths. The digger is the single deepest
        # tunnel, so if the selector ignored the digger exclusion it would be
        # picked first.
        for did, depth in ((10001, 500), (10002, 200), (10003, 150), (10004, 50)):
            dig_repo.create_tunnel(did, TEST_GUILD_ID, "T")
            dig_repo.update_tunnel(did, TEST_GUILD_ID, depth=depth)

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=10001,
            event_name="The Deep Hunter",
            strategy="deepest_n",
            victim_count=2,
            penalty_jc=10,
        )

        victim_ids = [vid for vid, _ in result.victims]
        # Digger is deepest but excluded; the two deepest non-diggers are hit.
        assert victim_ids == [10002, 10003]
        assert 10001 not in victim_ids
        # Each victim burned the deflation-strengthened penalty; shallower 10004 untouched.
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == 90
        assert player_repository.get_balance(10003, TEST_GUILD_ID) == 90
        assert player_repository.get_balance(10004, TEST_GUILD_ID) == 100
        # Digger's own balance is not touched by the splash.
        assert player_repository.get_balance(10001, TEST_GUILD_ID) == 100
        assert result.total_burned == 20
