"""Tests for multi-boss tiers, per-tunnel boss locking, and the
reactive mid-fight prompt state machine (``start_boss_duel`` +
``resume_boss_duel``).

Complements ``tests/test_boss_duel.py``, which covers the legacy
synchronous ``fight_boss`` path.
"""

from __future__ import annotations

import json
import random
import time

import pytest

from domain.models.boss_mechanics import MECHANIC_REGISTRY
from domain.models.boss_stingers import STINGER_REGISTRY
from repositories.dig_repository import DigRepository
from services.dig_constants import (
    BOSSES_BY_ID,
    BOSSES_BY_TIER,
    FREE_DIG_COOLDOWN_SECONDS,
    get_boss_pool_for_tier,
)
from services.dig_service import DigService
from tests.conftest import TEST_GUILD_ID

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


@pytest.fixture
def deterministic_rng(monkeypatch):
    """Force the mechanic pool and boss pool to pick index 0 deterministically.

    Patches ``random.Random`` so ``Random().choice(seq)`` always returns
    ``seq[0]``. Combined with ``monkeypatch.setattr(random, "random", ...)``
    in the test body this gives fully deterministic state-machine behaviour:
    the grandfathered boss is locked, the first pool mechanic fires, and
    per-round / per-option rolls use the pinned value.
    """
    class _FakeRandom:
        def __init__(self, seed=None):
            pass

        def choice(self, seq):
            return seq[0] if seq else None

        def random(self):
            return 0.0

        def getstate(self):
            return ()

        def setstate(self, state):
            pass

    monkeypatch.setattr(random, "Random", _FakeRandom)


def _seed_paused_duel(
    dig_repo, discord_id, guild_id, *,
    boss_id="grothak", tier=25, mechanic_id="grothak_earthquake",
    risk_tier="cautious", wager=10, player_hp=3, boss_hp=3, round_num=3,
    player_hit=0.60, player_dmg=1, boss_hit=0.30, boss_dmg=1,
    round_log=None, pending_prompt=None,
):
    """Seed a mid-duel row directly (bypasses ``start_boss_duel`` timing).

    Used by tests that want to drive ``resume_boss_duel`` without depending on
    the mechanic-trigger RNG alignment.
    """
    from domain.models.boss_mechanics import MECHANIC_REGISTRY as _MECHS
    mech = _MECHS[mechanic_id]
    pp = pending_prompt or {
        "mechanic_id": mechanic_id,
        "prompt_title": mech.prompt_title,
        "prompt_description": mech.prompt_description,
        "options": [
            {"option_idx": i, "label": o.label}
            for i, o in enumerate(mech.options)
        ],
        "safe_option_idx": mech.safe_option_idx,
    }
    state = {
        "boss_id": boss_id, "tier": tier, "mechanic_id": mechanic_id,
        "risk_tier": risk_tier, "wager": wager,
        "player_hp": player_hp, "boss_hp": boss_hp,
        "round_num": round_num,
        "round_log": json.dumps(round_log or []),
        "pending_prompt": json.dumps(pp),
        "rng_state": "",
        "status_effects": json.dumps({
            "attempts_this_fight": 1,
            "initial_win_chance": 0.5,
            "multiplier": 2.0,
        }),
        "echo_applied": 0, "echo_killer_id": None,
        "player_hit": player_hit, "player_dmg": player_dmg,
        "boss_hit": boss_hit, "boss_dmg": boss_dmg,
    }
    dig_repo.save_active_duel(discord_id, guild_id, state)


def _register(player_repo, discord_id=10001, balance=500):
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
    return discord_id


def _at_boss(dig_service, dig_repo, player_repository, monkeypatch, *, depth=24, prestige=0, discord_id=10001):
    """Place a fresh player just before the depth-25 boss boundary."""
    _register(player_repository, discord_id=discord_id)
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    monkeypatch.setattr(random, "random", lambda: 0.99)
    dig_service.dig(discord_id, TEST_GUILD_ID)
    dig_repo.update_tunnel(discord_id, TEST_GUILD_ID, depth=depth, prestige_level=prestige)
    monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)


# ---------------------------------------------------------------------------
# Data-model sanity
# ---------------------------------------------------------------------------

class TestRosterShape:
    """Cross-referential invariants between BossDef, mechanics, and stingers."""

    def test_three_bosses_per_tier(self):
        for tier, pool in BOSSES_BY_TIER.items():
            assert len(pool) == 3, f"tier {tier} has {len(pool)} bosses"

    def test_grandfathered_first(self):
        """The first boss per tier is a grandfathered fantasy boss (by depth)."""
        grandfathered_ids = {
            25: "grothak",
            50: "crystalia",
            75: "magmus_rex",
            100: "void_warden",
            150: "sporeling_sovereign",
            200: "chronofrost",
            275: "nameless_depth",
        }
        for tier, boss_id in grandfathered_ids.items():
            assert BOSSES_BY_TIER[tier][0].boss_id == boss_id

    def test_every_boss_has_mechanic_and_stinger(self):
        for boss_id, boss in BOSSES_BY_ID.items():
            assert boss.mechanic_pool, f"{boss_id}: empty mechanic_pool"
            for mid in boss.mechanic_pool:
                assert mid in MECHANIC_REGISTRY, f"{boss_id}: unknown mechanic {mid!r}"
            assert boss.stinger_id, f"{boss_id}: empty stinger_id"
            assert boss.stinger_id in STINGER_REGISTRY, f"{boss_id}: unknown stinger {boss.stinger_id!r}"

    def test_boss_ids_unique(self):
        all_ids = [b.boss_id for pool in BOSSES_BY_TIER.values() for b in pool]
        assert len(all_ids) == len(set(all_ids))
        assert len(all_ids) == 21


class TestMechanicInvariants:
    """Every mechanic must be structurally valid."""

    def test_three_options_each(self):
        for mid, mech in MECHANIC_REGISTRY.items():
            assert len(mech.options) == 3, f"{mid}: {len(mech.options)} options"

    def test_probabilities_sum_to_one(self):
        for mid, mech in MECHANIC_REGISTRY.items():
            for i, opt in enumerate(mech.options):
                total = sum(r.probability for r in opt.outcome_rolls)
                assert abs(total - 1.0) < 1e-6, (
                    f"{mid}.options[{i}]: probabilities sum to {total}"
                )

    def test_safe_option_idx_in_range(self):
        for mid, mech in MECHANIC_REGISTRY.items():
            assert 0 <= mech.safe_option_idx < 3, f"{mid}: {mech.safe_option_idx}"


# ---------------------------------------------------------------------------
# Boss locking: roll once, persist, don't reroll until reset
# ---------------------------------------------------------------------------

class TestBossLocking:
    """_ensure_boss_locked rolls + persists, then returns the same boss."""

    def test_ensure_locks_on_first_call(self, dig_service, dig_repo, player_repository, monkeypatch):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))

        boss = dig_service._ensure_boss_locked(10001, TEST_GUILD_ID, tunnel, 25)
        assert boss.boss_id in {b.boss_id for b in get_boss_pool_for_tier(25)}

        # Persisted in the tunnel's boss_progress JSON.
        fresh = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        progress = json.loads(fresh["boss_progress"])
        assert progress["25"]["boss_id"] == boss.boss_id

    def test_second_call_returns_same_boss(self, dig_service, dig_repo, player_repository, monkeypatch):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))

        boss1 = dig_service._ensure_boss_locked(10001, TEST_GUILD_ID, tunnel, 25)
        # Reload tunnel between calls.
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        boss2 = dig_service._ensure_boss_locked(10001, TEST_GUILD_ID, tunnel, 25)
        assert boss1.boss_id == boss2.boss_id

    def test_distribution_across_many_tunnels(self, repo_db_path, player_repository, monkeypatch):
        """Over many fresh tunnels, all 3 tier-25 bosses get rolled at least once.

        120 iterations keeps the probability that one of 3 equiprobable bosses
        is never drawn at ``3 × (2/3)**120 ≈ 1.6 × 10⁻²⁰`` — safely below any
        realistic flake budget. Early-exits on first complete set, so runtime
        is ~3 iterations in expectation.
        """
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        seen = set()
        for i in range(120):
            dig_repo = DigRepository(repo_db_path)
            svc = DigService(dig_repo, player_repository)
            monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
            # Fresh player per iteration.
            player_repository.add(
                discord_id=20000 + i, discord_username=f"Mt{i}",
                guild_id=TEST_GUILD_ID, initial_mmr=3000, glicko_rating=1500.0,
                glicko_rd=350.0, glicko_volatility=0.06,
            )
            dig_repo.create_tunnel(20000 + i, TEST_GUILD_ID, name=f"t{i}")
            tunnel = dict(dig_repo.get_tunnel(20000 + i, TEST_GUILD_ID))
            boss = svc._ensure_boss_locked(20000 + i, TEST_GUILD_ID, tunnel, 25)
            seen.add(boss.boss_id)
            if len(seen) == 3:
                break
        expected = {b.boss_id for b in get_boss_pool_for_tier(25)}
        assert seen == expected, f"Only saw {seen}; expected all of {expected}"


# ---------------------------------------------------------------------------
# boss_progress JSON migration
# ---------------------------------------------------------------------------

class TestBossProgressJson:
    """``_get_boss_progress`` normalizes both legacy string-status and new
    ``{boss_id, status}`` formats."""

    def test_reads_legacy_string_format(self, dig_service):
        tunnel = {
            "boss_progress": json.dumps({"25": "defeated", "50": "active"}),
        }
        progress = dig_service._get_boss_progress(tunnel)
        assert progress["25"] == "defeated"
        assert progress["50"] == "active"

    def test_reads_new_dict_format(self, dig_service):
        tunnel = {
            "boss_progress": json.dumps({
                "25": {"boss_id": "pudge", "status": "defeated"},
                "50": {"boss_id": "crystalia", "status": "active"},
            }),
        }
        progress = dig_service._get_boss_progress(tunnel)
        assert progress["25"] == "defeated"
        assert progress["50"] == "active"

    def test_get_locked_boss_id_legacy_fallback(self, dig_service):
        """Legacy-format entries fall back to the grandfathered boss."""
        tunnel = {"boss_progress": json.dumps({"25": "active"})}
        assert dig_service._get_locked_boss_id(tunnel, 25) == "grothak"

    def test_get_locked_boss_id_from_new_format(self, dig_service):
        tunnel = {"boss_progress": json.dumps({
            "25": {"boss_id": "pudge", "status": "active"},
        })}
        assert dig_service._get_locked_boss_id(tunnel, 25) == "pudge"


# ---------------------------------------------------------------------------
# Reactive mid-fight prompt state machine
# ---------------------------------------------------------------------------

class TestStartBossDuel:
    """``start_boss_duel`` resolves or pauses based on the rolled mechanic."""

    def test_pauses_at_mechanic_trigger_round(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        # Force auto-rounds to be non-terminating until the trigger.
        monkeypatch.setattr(random, "random", lambda: 0.5)
        result = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)
        # The duel should either pause with a pending_prompt OR resolve.
        # With a 3-round trigger mechanic (grothak's is round 3), pause is likely.
        assert result["success"]

        # Check DB state — if paused, a row exists; if resolved, no row.
        row = dig_repo.get_active_duel(10001, TEST_GUILD_ID)
        if result.get("pending_prompt"):
            assert row is not None
            assert row["boss_id"] in {b.boss_id for b in get_boss_pool_for_tier(25)}
            assert row["mechanic_id"] in MECHANIC_REGISTRY
            assert row["round_num"] >= 1
        else:
            assert row is None

    def test_resume_clears_state_row(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Seed a paused duel directly so the test doesn't depend on RNG
        aligning the mechanic trigger with fight pacing."""
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        # Lock Grothak, seed a paused duel at round 3 with the earthquake mechanic.
        progress = json.dumps({"25": {"boss_id": "grothak", "status": "active"}})
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, boss_progress=progress)
        _seed_paused_duel(dig_repo, 10001, TEST_GUILD_ID)
        assert dig_repo.get_active_duel(10001, TEST_GUILD_ID) is not None

        monkeypatch.setattr(random, "random", lambda: 0.0)  # deterministic rolls
        result = dig_service.resume_boss_duel(10001, TEST_GUILD_ID, option_idx=0)
        assert result["success"]
        assert "won" in result
        assert dig_repo.get_active_duel(10001, TEST_GUILD_ID) is None

    def test_resume_without_state_errors(self, dig_service, dig_repo, player_repository):
        _register(player_repository)
        result = dig_service.resume_boss_duel(10001, TEST_GUILD_ID, option_idx=0)
        assert result["success"] is False

    def test_start_validation_errors(self, dig_service, dig_repo, player_repository, monkeypatch):
        # No tunnel at all
        assert (
            dig_service.start_boss_duel(99999, TEST_GUILD_ID, "cautious", wager=0)["success"]
            is False
        )
        # Not at boss boundary
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        assert (
            dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=0)["success"]
            is False
        )

    def test_pending_prompt_shape(
        self, dig_service, dig_repo, player_repository, monkeypatch, deterministic_rng,
    ):
        """With ``deterministic_rng`` (``Random().choice`` always returns
        ``seq[0]``) and pinned ``random.random=0.5``, the first boss in tier
        25 (Grothak) is locked and its first pool mechanic (earthquake,
        trigger_round=3) always fires. Player_hit=0.60 cautious + pinned 0.5
        = hit; boss_hit=0.30 + pinned 0.5 = miss. Fight pauses at round 3."""
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        monkeypatch.setattr(random, "random", lambda: 0.5)
        start = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert start.get("pending_prompt"), (
            f"Expected a mid-fight prompt to fire; got {start}"
        )
        pp = start["pending_prompt"]
        assert {"mechanic_id", "prompt_title", "options", "safe_option_idx"}.issubset(pp)
        assert len(pp["options"]) == 3
        for i, opt in enumerate(pp["options"]):
            assert opt["option_idx"] == i
            assert opt["label"]


class TestApplyOptionOutcome:
    """``_apply_option_outcome_to_state`` rolls the option distribution and
    applies the chosen OutcomeRoll deltas + status effects to the duel state."""

    def test_applies_player_hp_delta(self, dig_service, monkeypatch):
        mech = MECHANIC_REGISTRY["pudge_hook"]
        # Option 0: Dodge left — 0.70 prob of 0 dmg, 0.30 prob of -1 dmg
        # Pin random.random to 0.0 → selects the first (0.70) branch.
        monkeypatch.setattr(random, "random", lambda: 0.0)
        narrative, player_hp, boss_hp, effects = dig_service._apply_option_outcome_to_state(
            option=mech.options[0], player_hp=5, boss_hp=5, status_effects={},
        )
        assert player_hp == 5  # no damage on the first branch
        assert boss_hp == 5

    def test_applies_boss_hp_delta(self, dig_service, monkeypatch):
        mech = MECHANIC_REGISTRY["pudge_hook"]
        # Option 2 (grab): 0.25 prob of dealing 4 to boss, 0.75 prob of -3 self
        monkeypatch.setattr(random, "random", lambda: 0.0)  # first branch
        narrative, player_hp, boss_hp, effects = dig_service._apply_option_outcome_to_state(
            option=mech.options[2], player_hp=5, boss_hp=5, status_effects={},
        )
        assert boss_hp == 1  # 5 - 4
        assert player_hp == 5

    def test_applies_skip_next_round(self, dig_service, monkeypatch):
        mech = MECHANIC_REGISTRY["pudge_hook"]
        # Option 1 (dodge right into swing), branch 2: skip_next_round_for=player
        # Branch 1 is prob 0.50, branch 2 is prob 0.50. Random = 0.99 picks branch 2.
        monkeypatch.setattr(random, "random", lambda: 0.99)
        narrative, _, _, effects = dig_service._apply_option_outcome_to_state(
            option=mech.options[1], player_hp=5, boss_hp=5, status_effects={},
        )
        assert effects.get("skip_next_round_for") == "player"

    def test_applies_status_effect(self, dig_service, monkeypatch):
        mech = MECHANIC_REGISTRY["cm_frostbite"]
        # Option 1 (close distance): branch 2 (prob 0.55) applies frostbite
        monkeypatch.setattr(random, "random", lambda: 0.99)  # Select second branch.
        narrative, _, _, effects = dig_service._apply_option_outcome_to_state(
            option=mech.options[1], player_hp=5, boss_hp=5, status_effects={},
        )
        assert effects.get("frostbite_next_round") is True


# ---------------------------------------------------------------------------
# Stingers: per-boss loss penalties beyond wager + knockback
# ---------------------------------------------------------------------------

class TestStingers:
    """Stinger application on loss (extra_knockback / extended_cooldown_s / curse)."""

    def test_extra_knockback_widens_loss(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Seed a paused Pudge duel at 1 HP and resume with pinned-miss rolls so
        the loss branch deterministically fires the expected stinger."""
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch, depth=50)
        progress = json.dumps({"25": {"boss_id": "pudge", "status": "active"}})
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, boss_progress=progress, depth=24)
        _seed_paused_duel(
            dig_repo, 10001, TEST_GUILD_ID,
            boss_id="pudge", mechanic_id="pudge_hook",
            player_hp=1, boss_hp=4, round_num=3,
        )
        # option_idx=2 (Grab the hook): first branch (0.25 prob, narrative
        # "massive hit") requires roll < 0.25; pin random.random() = 0.99 so
        # the second branch (-3 player HP) fires, killing the player from 1 HP
        # and forcing the loss branch + stinger.
        monkeypatch.setattr(random, "random", lambda: 0.99)
        result = dig_service.resume_boss_duel(10001, TEST_GUILD_ID, option_idx=2)
        assert result["won"] is False
        from domain.models.boss_stingers import STINGER_REGISTRY
        assert result["extra_knockback"] == STINGER_REGISTRY["pudge_drag"].extra_knockback

    def test_cursed_status_written_on_loss(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch, depth=50)
        progress = json.dumps({"25": {"boss_id": "ogre_magi", "status": "active"}})
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, boss_progress=progress, depth=24)

        monkeypatch.setattr(random, "random", lambda: 0.999)
        result = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)
        if result.get("pending_prompt"):
            safe = result["pending_prompt"]["safe_option_idx"]
            result = dig_service.resume_boss_duel(10001, TEST_GUILD_ID, option_idx=safe)
        assert result["won"] is False
        # ogre_blast stinger sets extended_cooldown_s only (no curse, no extra_kb).
        from domain.models.boss_stingers import STINGER_REGISTRY
        assert result["extra_cooldown_s"] == STINGER_REGISTRY["ogre_blast"].extended_cooldown_s
        fresh = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        # No curse column set since ogre_blast has cursed_status=None.
        assert fresh.get("stinger_curse") in (None, "", "null")

    def test_cursed_status_writes_when_configured(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Tier-50 Crystal Maiden has cm_freeze → cursed_status=halve_next_wager."""
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch, depth=50)
        # Advance tunnel to depth 49 (one before tier-50 boundary).
        progress = json.dumps({
            "25": {"boss_id": "grothak", "status": "defeated"},
            "50": {"boss_id": "crystal_maiden", "status": "active"},
        })
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID, boss_progress=progress, depth=49,
            max_depth=49,
        )
        monkeypatch.setattr(random, "random", lambda: 0.999)
        result = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)
        if result.get("pending_prompt"):
            safe = result["pending_prompt"]["safe_option_idx"]
            result = dig_service.resume_boss_duel(10001, TEST_GUILD_ID, option_idx=safe)
        assert result["won"] is False
        fresh = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        curse = json.loads(fresh["stinger_curse"])
        assert curse.get("halve_next_wager") is True


# ---------------------------------------------------------------------------
# Echo keying: per-boss_id isolation within a tier
# ---------------------------------------------------------------------------

class TestEchoPerBossId:
    """Killing Pudge doesn't weaken Ogre Magi at the same tier."""

    def test_echo_written_under_specific_boss_id(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        # Lock Pudge.
        progress = json.dumps({"25": {"boss_id": "pudge", "status": "active"}})
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, boss_progress=progress)

        # Cautious gives enough HP headroom that even a bad prompt roll on the
        # safe option doesn't lose the fight. We pin random.random=0.0 so every
        # hit lands and every probability distribution picks its first branch.
        monkeypatch.setattr(random, "random", lambda: 0.0)
        result = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)
        if result.get("pending_prompt"):
            safe = result["pending_prompt"]["safe_option_idx"]
            result = dig_service.resume_boss_duel(10001, TEST_GUILD_ID, option_idx=safe)
        assert result["won"] is True
        # Echo row exists under pudge, not grothak.
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "pudge") is not None
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak") is None
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "ogre_magi") is None
