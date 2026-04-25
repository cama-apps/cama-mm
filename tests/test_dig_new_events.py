"""Tests for the risky cross-player event expansion + bumped event rates."""

from __future__ import annotations

import datetime
import random

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import EVENT_POOL, RANDOM_EVENTS
from services.dig_splash import resolve_splash
from tests.conftest import TEST_GUILD_ID

NEW_EVENT_IDS = (
    "aegis_whisper",
    "echoing_mime",
    "crow_snipe",
    "smoke_detour",
    "strangers_lamp",
    "drill_sergeant",
    "pit_lords_toll",
    "damned_bottle",
    "roshpit_gambit",
    "wilderness_stalker",
)


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


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
    with player_repo.connection() as conn:
        conn.cursor().execute(
            "UPDATE players SET last_match_date = ? WHERE discord_id = ? AND guild_id = ?",
            (datetime.datetime.now(datetime.UTC).isoformat(), discord_id, TEST_GUILD_ID),
        )


class TestNewEventsRegistered:
    """All 10 new events should appear in RANDOM_EVENTS and the serialized EVENT_POOL."""

    def test_all_new_events_in_random_events(self):
        ids = {e.id for e in RANDOM_EVENTS}
        for new_id in NEW_EVENT_IDS:
            assert new_id in ids, f"{new_id} missing from RANDOM_EVENTS"

    def test_all_new_events_in_event_pool(self):
        pool_ids = {e["id"] for e in EVENT_POOL}
        for new_id in NEW_EVENT_IDS:
            assert new_id in pool_ids, f"{new_id} missing from EVENT_POOL"

    def test_all_new_events_have_multiple_descriptions(self):
        by_id = {e.id: e for e in RANDOM_EVENTS}
        for new_id in NEW_EVENT_IDS:
            event = by_id[new_id]
            assert len(event.description) >= 2, (
                f"{new_id} should have ≥2 description variants for flavor variety"
            )


class TestStealMode:
    """The new mode='steal' transfers JC from victim to digger via steal_atomic."""

    def test_steal_credits_digger_and_debits_victim(
        self, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 20001, balance=100)  # digger
        _register(player_repository, 20002, balance=200)  # victim
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=20001,
            event_name="Steal Test",
            strategy="random_active",
            victim_count=1,
            penalty_jc=15,
            mode="steal",
        )
        assert result.mode == "steal"
        assert result.victims == [(20002, 15)]
        # Victim is debited, digger is credited atomically.
        assert player_repository.get_balance(20001, TEST_GUILD_ID) == 115
        assert player_repository.get_balance(20002, TEST_GUILD_ID) == 185

    def test_steal_logs_both_victim_and_thief(
        self, dig_repo, player_repository, monkeypatch,
    ):
        """Audit trail must include the digger's credit, not just the victim's debit."""
        _register(player_repository, 20001, balance=100)  # digger
        _register(player_repository, 20002, balance=200)  # victim
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=20001,
            event_name="Audit Test",
            strategy="random_active",
            victim_count=1,
            penalty_jc=15,
            mode="steal",
        )
        with dig_repo.connection() as conn:
            rows = conn.cursor().execute(
                "SELECT actor_id, action_type, jc_delta FROM dig_actions "
                "WHERE action_type IN ('splash_victim', 'splash_thief') "
                "ORDER BY id",
            ).fetchall()
        actions = [(r["actor_id"], r["action_type"], r["jc_delta"]) for r in rows]
        assert (20002, "splash_victim", -15) in actions
        assert (20001, "splash_thief", 15) in actions

    def test_steal_can_push_victim_below_zero(
        self, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 20001, balance=0)  # digger
        _register(player_repository, 20002, balance=5)  # victim with low balance
        monkeypatch.setattr(random, "sample", lambda pool, k: pool[:k])

        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=20001,
            event_name="Deep Steal",
            strategy="random_active",
            victim_count=1,
            penalty_jc=15,
            mode="steal",
        )
        # Steal is unclamped on the victim (matches Red/Blue Shell semantics).
        assert result.victims == [(20002, 15)]
        assert player_repository.get_balance(20001, TEST_GUILD_ID) == 15
        assert player_repository.get_balance(20002, TEST_GUILD_ID) == -10

    def test_steal_with_empty_pool_returns_empty(self, dig_repo, player_repository):
        _register(player_repository, 20001, balance=100)  # only the digger
        result = resolve_splash(
            player_repo=player_repository,
            dig_repo=dig_repo,
            guild_id=TEST_GUILD_ID,
            digger_id=20001,
            event_name="Lonely Steal",
            strategy="random_active",
            victim_count=1,
            penalty_jc=10,
            mode="steal",
        )
        assert result.victims == []
        assert result.total_burned == 0
        # Digger balance unchanged when no victims to steal from.
        assert player_repository.get_balance(20001, TEST_GUILD_ID) == 100


class TestSplashSerialization:
    """_splash_to_dict must propagate `mode` so the embed renders the right copy."""

    def test_mode_round_trips_through_splash_to_dict(self):
        from services.dig_service import _splash_to_dict
        from services.dig_splash import SplashResult

        for mode in ("burn", "grant", "steal"):
            result = SplashResult(
                strategy="random_active",
                event_name="Mode Test",
                victims=[(99001, 10)],
                total_burned=10,
                mode=mode,
            )
            d = _splash_to_dict(result)
            assert d is not None
            assert d.get("mode") == mode, (
                f"_splash_to_dict dropped mode={mode!r}; "
                "embed rendering will silently fall back to 'burn'"
            )


class TestEventRatesBumped:
    """Regression guard against accidentally reverting the +25% frequency bump."""

    def test_event_rates_bumped(self):
        # Read both event_rates dict literals from dig_service.py (there are two
        # parallel rolling paths). They must stay in sync.
        from pathlib import Path
        src = Path(__file__).parent.parent / "services" / "dig_service.py"
        text = src.read_text()
        # Both copies should contain the bumped values.
        assert text.count('"Dirt": 0.25') == 2, (
            "expected both event_rates dicts to use Dirt=0.25"
        )
        assert text.count('"The Hollow": 0.50') == 2, (
            "expected both event_rates dicts to use The Hollow=0.50"
        )
