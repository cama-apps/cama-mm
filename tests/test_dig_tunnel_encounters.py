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
from utils.economy_scaling import (
    scale_deflationary_minigame_jc_delta,
    scale_minigame_jc_delta,
)

# Authored spec for each encounter event, mirrored from the design table so
# the invariant test compares against intent rather than reading the value
# back from the same dict it's checking (which would be a tautology).
EXPECTED_EVENTS = {
    "hungering_dark": {
        "rarity": "uncommon", "min_depth": 10, "strategy": "richest_n",
        "victim_count": 2, "penalty_jc": 5, "payout": 5,
    },
    "deny_the_seam": {
        "rarity": "uncommon", "min_depth": 20, "strategy": "richest_n",
        "victim_count": 2, "penalty_jc": 6, "payout": 6,
    },
    "turf_war": {
        "rarity": "uncommon", "min_depth": 35, "strategy": "active_diggers",
        "victim_count": 3, "penalty_jc": 6, "payout": 8,
    },
    "smoke_ambush": {
        "rarity": "rare", "min_depth": 60, "strategy": "active_diggers",
        "victim_count": 3, "penalty_jc": 7, "payout": 10,
    },
    "the_tear": {
        "rarity": "rare", "min_depth": 90, "strategy": "random_active",
        "victim_count": 3, "penalty_jc": 8, "payout": 10,
    },
    "the_deep_hunter": {
        "rarity": "rare", "min_depth": 110, "strategy": "deepest_n",
        "victim_count": 2, "penalty_jc": 12, "payout": 12,
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
        # the +-50% JC jitter so the payout lands at the scaled authored 5.
        monkeypatch.setattr(random, "random", lambda: 0.01)
        monkeypatch.setattr(random, "uniform", lambda a, b: 1.0)

        result = dig_service.resolve_event(digger, guild_id, "hungering_dark", "risky")

        # Outcome wiring.
        assert result["success"] is True
        assert result["succeeded"] is True
        payout = scale_minigame_jc_delta(5)
        burn = scale_deflationary_minigame_jc_delta(5)
        assert result["jc_delta"] == payout
        assert result["splash"] is not None
        assert result["splash"]["mode"] == "burn"

        # The two RICHEST others are burned; the third is untouched.
        assert player_repository.get_balance(10002, guild_id) == 1000 - burn
        assert player_repository.get_balance(10003, guild_id) == 800 - burn
        assert player_repository.get_balance(10004, guild_id) == 500
        assert player_repository.get_balance(digger, guild_id) == 50 + payout

        # Globally deflationary: digger payout is smaller than the total burn.
        total_after = sum(
            player_repository.get_balance(pid, guild_id)
            for pid in (digger, 10002, 10003, 10004)
        )
        assert total_after == total_before + payout - (2 * burn)

    def test_empty_pool_pays_nothing(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Proportional payout closes the silent net-inflation hole: with no
        eligible victims the burn is 0, so the digger is paid 0 — the event
        cannot mint coin against an empty/poor pool."""
        digger = 10001
        _register(player_repository, digger, guild_id, 50)  # only player present
        dig_repo.create_tunnel(digger, guild_id, "T")
        dig_repo.update_tunnel(digger, guild_id, depth=120)

        monkeypatch.setattr(random, "random", lambda: 0.01)   # risky success
        monkeypatch.setattr(random, "uniform", lambda a, b: 1.0)

        result = dig_service.resolve_event(digger, guild_id, "hungering_dark", "risky")

        assert result["succeeded"] is True
        # richest_n found no other positive-balance players -> nothing burned ->
        # payout scaled to 0. No coin minted.
        assert result["jc_delta"] == 0
        assert result["splash"] is None
        assert player_repository.get_balance(digger, guild_id) == 50

    def test_low_balance_burn_targets_are_threshold_protected(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Sub-50 victims are skipped, so no splash payout can mint against them."""
        digger = 10001
        _register(player_repository, digger, guild_id, 50)
        # The only two other players are below the AOE-drain threshold.
        _register(player_repository, 10002, guild_id, 3)
        _register(player_repository, 10003, guild_id, 3)
        dig_repo.create_tunnel(digger, guild_id, "T")
        dig_repo.update_tunnel(digger, guild_id, depth=120)

        total_before = sum(
            player_repository.get_balance(p, guild_id) for p in (digger, 10002, 10003)
        )
        monkeypatch.setattr(random, "random", lambda: 0.01)
        monkeypatch.setattr(random, "uniform", lambda a, b: 1.0)

        result = dig_service.resolve_event(digger, guild_id, "hungering_dark", "risky")

        assert result["jc_delta"] == 0
        assert result["splash"] is None
        assert player_repository.get_balance(10002, guild_id) == 3
        assert player_repository.get_balance(10003, guild_id) == 3
        assert player_repository.get_balance(digger, guild_id) == 50
        total_after = sum(
            player_repository.get_balance(p, guild_id) for p in (digger, 10002, 10003)
        )
        assert total_after == total_before

    def test_failure_does_not_burn(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """trigger='success' means a failed risky pick burns nobody; the digger
        eats the authored failure loss and no splash fires."""
        digger = 10001
        _register(player_repository, digger, guild_id, 50)
        _register(player_repository, 10002, guild_id, 1000)
        dig_repo.create_tunnel(digger, guild_id, "T")
        dig_repo.update_tunnel(digger, guild_id, depth=120)

        monkeypatch.setattr(random, "random", lambda: 0.99)   # > 0.60 => failure
        monkeypatch.setattr(random, "uniform", lambda a, b: 1.0)

        result = dig_service.resolve_event(digger, guild_id, "hungering_dark", "risky")
        assert result["succeeded"] is False
        assert result["splash"] is None
        # Authored failure -5 is first tuned by the negative multiplier, then
        # deflation-strengthened before economy scaling.
        assert result["jc_delta"] == scale_deflationary_minigame_jc_delta(-6)
        assert player_repository.get_balance(10002, guild_id) == 1000  # untouched

    def test_active_diggers_burn_full_when_funded(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """turf_war: funded recent diggers each lose scaled JC and the digger gets scaled payout."""
        digger = 10001
        _register(player_repository, digger, guild_id, 50)
        for vid in (10002, 10003, 10004):
            _register(player_repository, vid, guild_id, 100)
            dig_repo.log_action(actor_id=vid, guild_id=guild_id, action_type="dig", detail={})
        dig_repo.create_tunnel(digger, guild_id, "T")
        dig_repo.update_tunnel(digger, guild_id, depth=120)

        total_before = sum(
            player_repository.get_balance(p, guild_id)
            for p in (digger, 10002, 10003, 10004)
        )
        monkeypatch.setattr(random, "random", lambda: 0.01)
        monkeypatch.setattr(random, "uniform", lambda a, b: 1.0)
        monkeypatch.setattr(random, "sample", lambda pop, k: list(pop)[:k])

        result = dig_service.resolve_event(digger, guild_id, "turf_war", "risky")
        assert result["succeeded"] is True
        payout = scale_minigame_jc_delta(8)
        burn = scale_deflationary_minigame_jc_delta(6)
        assert result["jc_delta"] == payout
        for vid in (10002, 10003, 10004):
            assert player_repository.get_balance(vid, guild_id) == 100 - burn
        assert player_repository.get_balance(digger, guild_id) == 50 + payout
        total_after = sum(
            player_repository.get_balance(p, guild_id)
            for p in (digger, 10002, 10003, 10004)
        )
        assert total_after == total_before + payout - (3 * burn)

    def test_random_active_burn_full_when_funded(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """the_tear: funded active players each lose scaled JC and the digger gets scaled payout."""
        import datetime
        digger = 10001
        _register(player_repository, digger, guild_id, 50)
        for vid in (10002, 10003, 10004):
            _register(player_repository, vid, guild_id, 100)
        # random_active draws from the lottery pool, which needs last_match_date.
        with player_repository.connection() as conn:
            conn.cursor().execute(
                "UPDATE players SET last_match_date = ? WHERE guild_id = ?",
                (datetime.datetime.now(datetime.UTC).isoformat(), guild_id),
            )
        dig_repo.create_tunnel(digger, guild_id, "T")
        dig_repo.update_tunnel(digger, guild_id, depth=120)

        total_before = sum(
            player_repository.get_balance(p, guild_id)
            for p in (digger, 10002, 10003, 10004)
        )
        monkeypatch.setattr(random, "random", lambda: 0.01)
        monkeypatch.setattr(random, "uniform", lambda a, b: 1.0)
        monkeypatch.setattr(random, "sample", lambda pop, k: list(pop)[:k])

        result = dig_service.resolve_event(digger, guild_id, "the_tear", "risky")
        assert result["succeeded"] is True
        payout = scale_minigame_jc_delta(10)
        burn = scale_deflationary_minigame_jc_delta(8)
        assert result["jc_delta"] == payout
        burned = sum(
            100 - player_repository.get_balance(v, guild_id) for v in (10002, 10003, 10004)
        )
        assert burned == 3 * burn
        assert player_repository.get_balance(digger, guild_id) == 50 + payout
        total_after = sum(
            player_repository.get_balance(p, guild_id)
            for p in (digger, 10002, 10003, 10004)
        )
        assert total_after == total_before + payout - burned
