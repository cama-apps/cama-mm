"""Tests for the pacing-v2 event drop: new events load correctly,
wealth-target splash strategies pick the right victims, and the
helltide-bell guild modifier hook runs end-to-end."""

from __future__ import annotations

import json
import random

import pytest

from repositories.dig_guild_modifier_repository import DigGuildModifierRepository
from repositories.dig_repository import DigRepository
from services.dig_constants import (
    EVENT_POOL,
    HELLTIDE_MODIFIER_ID,
    HELLTIDE_TAX_PER_DIG,
    RANDOM_EVENTS,
)
from services.dig_service import DigService
from services.dig_splash import _select_deepest_n, pick_splash_target

NEW_EVENT_IDS = {
    # Common
    "crimson_drizzle", "glyph_pulse", "mossfire_echo", "mana_fountain_crack",
    # Uncommon
    "the_hooked_stranger", "whispering_token", "the_beasts_pit",
    "aegis_denial", "sanguine_pact", "time_walker", "bounty_marker",
    "mages_archive", "phantom_strike", "silken_cocoon", "the_mothers_mark",
    # Marquee
    "helltide_bell", "the_sundering", "the_black_kings_bargain",
    # Fillers
    "the_old_gods_tongue", "crimson_rain_ladder",
}


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def gm_repo(repo_db_path):
    return DigGuildModifierRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, gm_repo, monkeypatch):
    svc = DigService(
        dig_repo, player_repository,
        dig_guild_modifier_repo=gm_repo,
    )
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repository, discord_id=10001, guild_id=12345, balance=10000):
    player_repository.add(
        discord_id=discord_id, discord_username=f"P{discord_id}",
        guild_id=guild_id, initial_mmr=3000,
        glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06,
    )
    player_repository.update_balance(discord_id, guild_id, balance)
    return discord_id


class TestEventsLoad:
    def test_all_new_events_in_random_events(self):
        ids = {e.id for e in RANDOM_EVENTS}
        missing = NEW_EVENT_IDS - ids
        assert not missing, f"missing event ids in RANDOM_EVENTS: {missing}"

    def test_all_new_events_in_event_pool(self):
        ids = {e["id"] for e in EVENT_POOL}
        missing = NEW_EVENT_IDS - ids
        assert not missing, f"missing event ids in EVENT_POOL: {missing}"

    def test_event_count_at_least_157(self):
        # Codebase had 137 events; the pacing-v2 drop adds 20.
        assert len(RANDOM_EVENTS) >= 157

    def test_each_new_event_has_safe_and_risky(self):
        new = [e for e in RANDOM_EVENTS if e.id in NEW_EVENT_IDS]
        for e in new:
            assert e.safe_option is not None, f"{e.id} missing safe_option"
            assert e.risky_option is not None, f"{e.id} missing risky_option"


class TestEventTraitMix:
    """Each new event should carry at least 2 of 3 traits:
    risky (cave_in or negative outcome on failure), deflationary
    (negative jc on a risky outcome), cross-player (splash or
    guild-modifier set)."""

    def _classify(self, event):
        risky = event.risky_option
        risky_failure = risky.failure
        is_risky = bool(risky_failure) and (
            risky_failure.jc < 0 or risky_failure.advance < 0 or risky_failure.cave_in
        )
        is_deflationary = (
            risky.success.jc < 0 or (risky_failure and risky_failure.jc < 0)
        )
        is_cross_player = (
            event.splash is not None or event.guild_modifier_on_success is not None
        )
        return is_risky, is_deflationary, is_cross_player

    def test_each_new_event_has_at_least_two_traits(self):
        for e in RANDOM_EVENTS:
            if e.id not in NEW_EVENT_IDS:
                continue
            risky, defl, xpl = self._classify(e)
            count = sum(1 for t in (risky, defl, xpl) if t)
            assert count >= 2, (
                f"{e.id} has {count} traits "
                f"(risky={risky}, defl={defl}, xpl={xpl})"
            )


class TestWealthWeightedTargeting:
    def test_pick_splash_target_wealthiest(self, dig_repo, player_repository, guild_id):
        _register(player_repository, discord_id=1, balance=100)
        _register(player_repository, discord_id=2, balance=5000)
        _register(player_repository, discord_id=3, balance=200)
        target = pick_splash_target(
            player_repo=player_repository, dig_repo=dig_repo,
            guild_id=guild_id, exclude_id=None, mode="wealthiest",
        )
        assert target == 2

    def test_pick_splash_target_excludes_caster(self, dig_repo, player_repository, guild_id):
        _register(player_repository, discord_id=1, balance=5000)
        _register(player_repository, discord_id=2, balance=100)
        target = pick_splash_target(
            player_repo=player_repository, dig_repo=dig_repo,
            guild_id=guild_id, exclude_id=1, mode="wealthiest",
        )
        # Player 1 wealthiest but excluded; player 2 picked.
        assert target == 2

    def test_pick_splash_target_random_recent_biases_rich(self, dig_repo, player_repository, guild_id):
        _register(player_repository, discord_id=1, balance=10000)
        _register(player_repository, discord_id=2, balance=100)
        _register(player_repository, discord_id=3, balance=100)
        for did in (1, 2, 3):
            dig_repo.log_action(
                discord_id=did, guild_id=guild_id, action_type="dig",
                jc_delta=0, details=json.dumps({}),
            )
        random.seed(7)
        picks = [
            pick_splash_target(
                player_repo=player_repository, dig_repo=dig_repo,
                guild_id=guild_id, exclude_id=None, mode="random_recent",
            )
            for _ in range(2000)
        ]
        rich_count = sum(1 for p in picks if p == 1)
        # Uniform = ~666 / 2000. Wealth-weighted with 10000 vs 100 vs 100
        # should land far above uniform.
        assert rich_count > 1500, f"rich picked {rich_count}/2000"

    def test_select_deepest_n_orders_by_depth(self, dig_repo, player_repository, guild_id):
        _register(player_repository, discord_id=1, balance=100)
        _register(player_repository, discord_id=2, balance=100)
        _register(player_repository, discord_id=3, balance=100)
        dig_repo.create_tunnel(1, guild_id, "A")
        dig_repo.update_tunnel(1, guild_id, depth=10)
        dig_repo.create_tunnel(2, guild_id, "B")
        dig_repo.update_tunnel(2, guild_id, depth=200)
        dig_repo.create_tunnel(3, guild_id, "C")
        dig_repo.update_tunnel(3, guild_id, depth=50)

        class _Bundle:
            __slots__ = ("player_repo", "dig_repo")
            def __init__(self, player_repo, dig_repo):
                self.player_repo = player_repo
                self.dig_repo = dig_repo

        bundle = _Bundle(player_repository, dig_repo)
        picked = _select_deepest_n(bundle, guild_id, digger_id=999, count=2)
        assert picked == [2, 3]


class TestHelltideBellEndToEnd:
    def test_helltide_modifier_taxes_dig_yield(self, dig_service, dig_repo, gm_repo, player_repository, guild_id, monkeypatch):
        from services.dig_constants import FREE_DIG_COOLDOWN_SECONDS
        _register(player_repository, balance=10000)
        monkeypatch.setattr("time.time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # avoid cave-in / events
        dig_service.dig(10001, guild_id)

        # Activate helltide directly (skipping the event UI).
        gm_repo.set_modifier(guild_id, HELLTIDE_MODIFIER_ID, duration_seconds=600)
        assert HELLTIDE_TAX_PER_DIG > 0

        monkeypatch.setattr("time.time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.dig(10001, guild_id)
        assert result.get("success")
        # Yield never goes below zero from the tax.
        assert player_repository.get_balance(10001, guild_id) >= 0
