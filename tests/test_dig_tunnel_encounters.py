"""Tests for the six deflationary player-interaction /dig events.

These "tunnel encounter" events burn jopacoin from OTHER players when the
digger succeeds at the risky choice, acting as a global deflation lever. The
tests pin down (1) the per-event splash spec and the net-deflation invariant
on the EVENT_POOL definitions, and (2) the end-to-end wiring through
``DigService.resolve_event`` for the deterministic ``richest_n`` strategy.
"""

import random

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import EVENT_POOL
from services.dig_service import DigService

# Authored spec for each encounter event, mirrored from the design table so
# the invariant test compares against intent rather than reading the value
# back from the same dict it's checking (which would be a tautology).
EXPECTED_EVENTS = {
    "hungering_dark": {
        "rarity": "uncommon", "min_depth": 10, "strategy": "richest_n",
        "victim_count": 2, "penalty_jc": 4, "payout": 5,
    },
    "deny_the_seam": {
        "rarity": "uncommon", "min_depth": 20, "strategy": "richest_n",
        "victim_count": 2, "penalty_jc": 5, "payout": 6,
    },
    "turf_war": {
        "rarity": "uncommon", "min_depth": 35, "strategy": "active_diggers",
        "victim_count": 3, "penalty_jc": 5, "payout": 8,
    },
    "smoke_ambush": {
        "rarity": "rare", "min_depth": 60, "strategy": "active_diggers",
        "victim_count": 3, "penalty_jc": 6, "payout": 10,
    },
    "the_tear": {
        "rarity": "rare", "min_depth": 90, "strategy": "random_active",
        "victim_count": 3, "penalty_jc": 7, "payout": 10,
    },
    "the_deep_hunter": {
        "rarity": "rare", "min_depth": 110, "strategy": "deepest_n",
        "victim_count": 2, "penalty_jc": 10, "payout": 12,
    },
}

VALID_STRATEGIES = {"richest_n", "active_diggers", "random_active", "deepest_n"}

_POOL_BY_ID = {e["id"]: e for e in EVENT_POOL}


class TestEncounterEventInvariants:
    """Definition-level invariants over the six encounter events."""

    @pytest.mark.parametrize("event_id", list(EXPECTED_EVENTS))
    def test_event_present_in_pool(self, event_id):
        assert event_id in _POOL_BY_ID, f"{event_id} missing from EVENT_POOL"

    @pytest.mark.parametrize("event_id", list(EXPECTED_EVENTS))
    def test_splash_is_a_success_triggered_burn(self, event_id):
        """All six destroy coin (mode=burn) only on a risky-success outcome,
        and are flagged social so they render as player-interaction events."""
        e = _POOL_BY_ID[event_id]
        splash = e["splash"]
        assert splash is not None, f"{event_id} has no splash config"
        assert splash["mode"] == "burn", f"{event_id} splash mode must be burn"
        assert splash["trigger"] == "success", f"{event_id} must fire on success"
        assert e.get("social") is True, f"{event_id} must be social"
        assert splash["strategy"] in VALID_STRATEGIES, (
            f"{event_id} has unknown strategy {splash['strategy']!r}"
        )

    @pytest.mark.parametrize("event_id", list(EXPECTED_EVENTS))
    def test_burn_dominates_payout_after_jitter(self, event_id):
        """Net-deflation invariant: the burned pool (victim_count * penalty_jc)
        must exceed 1.5x the digger's authored payout. The engine jitters the
        digger's JC by up to +50% (random.uniform(0.5, 1.5)); requiring the
        burn to clear the *maximum* jittered payout guarantees the event stays
        globally deflationary even on the luckiest roll."""
        e = _POOL_BY_ID[event_id]
        splash = e["splash"]
        burned = splash["victim_count"] * splash["penalty_jc"]
        payout = e["risky_option"]["success"]["jc"]
        assert burned >= 1.5 * payout, (
            f"{event_id}: burn {burned} does not dominate 1.5x payout "
            f"{1.5 * payout} (payout={payout})"
        )

    @pytest.mark.parametrize("event_id", list(EXPECTED_EVENTS))
    def test_matches_authored_spec(self, event_id):
        """Pin every field to the design table so a silent retune of rarity,
        depth gate, splash strategy/size, penalty, or payout fails loudly."""
        e = _POOL_BY_ID[event_id]
        spec = EXPECTED_EVENTS[event_id]
        splash = e["splash"]
        assert e["rarity"] == spec["rarity"], f"{event_id} rarity"
        assert e["min_depth"] == spec["min_depth"], f"{event_id} min_depth"
        assert splash["strategy"] == spec["strategy"], f"{event_id} strategy"
        assert splash["victim_count"] == spec["victim_count"], f"{event_id} victim_count"
        assert splash["penalty_jc"] == spec["penalty_jc"], f"{event_id} penalty_jc"
        assert e["risky_option"]["success"]["jc"] == spec["payout"], f"{event_id} payout"


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repository, discord_id, guild_id, balance):
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"U{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(discord_id, guild_id, balance)


class TestEncounterEndToEnd:
    """End-to-end wiring + net-deflation through resolve_event.

    Uses ``hungering_dark`` because ``richest_n`` selects deterministically
    (top-2 by balance, no random.sample), so the test needs no RNG control
    over victim selection — only the success roll and the JC jitter."""

    def test_richest_n_burn_is_net_deflationary(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        digger = 10001
        # Victims ordered by descending balance: 10002 > 10003 > 10004.
        _register(player_repository, digger, guild_id, 50)
        _register(player_repository, 10002, guild_id, 1000)
        _register(player_repository, 10003, guild_id, 800)
        _register(player_repository, 10004, guild_id, 500)

        dig_repo.create_tunnel(digger, guild_id, "T")
        dig_repo.update_tunnel(digger, guild_id, depth=120)

        balances_before = {
            pid: player_repository.get_balance(pid, guild_id)
            for pid in (digger, 10002, 10003, 10004)
        }
        total_before = sum(balances_before.values())

        # 0.01 < 0.60 success_chance => risky success. uniform->1.0 disables
        # the +-50% JC jitter so the payout lands at exactly the authored 5.
        monkeypatch.setattr(random, "random", lambda: 0.01)
        monkeypatch.setattr(random, "uniform", lambda a, b: 1.0)

        result = dig_service.resolve_event(digger, guild_id, "hungering_dark", "risky")

        # Outcome wiring.
        assert result["success"] is True
        assert result["succeeded"] is True
        assert result["jc_delta"] == 5
        assert result["splash"] is not None
        assert result["splash"]["mode"] == "burn"

        # The two RICHEST others are burned 4 each; the third is untouched.
        assert player_repository.get_balance(10002, guild_id) == 996
        assert player_repository.get_balance(10003, guild_id) == 796
        assert player_repository.get_balance(10004, guild_id) == 500
        # Digger keeps the +5 payout.
        assert player_repository.get_balance(digger, guild_id) == 55

        # Globally deflationary: digger +5, burned 8 => net -3 across the guild.
        total_after = sum(
            player_repository.get_balance(pid, guild_id)
            for pid in (digger, 10002, 10003, 10004)
        )
        assert total_after == total_before - 3
