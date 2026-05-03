"""Hard depth wall at PRESTIGE_HARD_CAP and accelerated luminosity drain
between LUMINOSITY_DEEP_DRAIN_START_DEPTH and the cap. Together these
push players to prestige instead of tunneling indefinitely past pinnacle.
"""

from __future__ import annotations

import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP,
    LUMINOSITY_DEEP_DRAIN_START_DEPTH,
    LUMINOSITY_DRAIN_PER_DIG,
    PRESTIGE_HARD_CAP,
)
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _seed_player(dig_service, dig_repo, player_repository, depth: int, luminosity: int = 100):
    import json as _json

    from services.dig_constants import BOSS_BOUNDARIES, PINNACLE_DEPTH

    player_repository.add(
        discord_id=10001,
        discord_username="Player10001",
        guild_id=12345,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(10001, 12345, 10000)
    random.seed(0)
    dig_service.dig(10001, 12345)
    # Mark all bosses (tier + pinnacle) defeated so deep-depth digs are
    # not intercepted by boss-skip catch-up. These tests measure
    # luminosity drain and the hard cap, not boss flow.
    bp = {str(b): "defeated" for b in BOSS_BOUNDARIES}
    bp[str(PINNACLE_DEPTH)] = "defeated"
    # Cooldown bypass: zero out last_dig_at so subsequent digs can run.
    # Pin last_lum_update_at to the mocked clock so any lazy-decay path
    # sees zero elapsed time and the luminosity invariant remains a
    # clean check on dig-time drain only.
    dig_repo.update_tunnel(
        10001, 12345,
        depth=depth, luminosity=luminosity,
        last_dig_at=0, last_lum_update_at=1_000_000,
        boss_progress=_json.dumps(bp),
    )


class TestPrestigeHardCap:
    def test_dig_at_cap_is_rejected(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A player at PRESTIGE_HARD_CAP cannot dig further — error is
        flavor-only, no cooldown burned, no luminosity drain, no JC."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_player(dig_service, dig_repo, player_repository, depth=PRESTIGE_HARD_CAP)
        bal_before = player_repository.get_balance(10001, 12345)
        tunnel_before = dict(dig_repo.get_tunnel(10001, 12345))

        result = dig_service.dig(10001, 12345)

        assert not result["success"]
        assert result.get("hard_cap") is True
        # Flavor message — no depth number, no command hint
        assert "yield" in result["error"].lower() or "ascen" in result["error"].lower()
        # Hard-cap contract: nothing is consumed by the rejected dig.
        assert player_repository.get_balance(10001, 12345) == bal_before
        tunnel_after = dict(dig_repo.get_tunnel(10001, 12345))
        assert tunnel_after["depth"] == tunnel_before["depth"]
        assert tunnel_after["luminosity"] == tunnel_before["luminosity"]
        assert tunnel_after["last_dig_at"] == tunnel_before["last_dig_at"]

    def test_dig_one_below_cap_allowed(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """At depth cap-1 the player can still dig (the wall fires at the
        cap, not before)."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_player(dig_service, dig_repo, player_repository, depth=PRESTIGE_HARD_CAP - 1)
        random.seed(99)
        result = dig_service.dig(10001, 12345)
        # Either succeeded or was blocked by some other reason — must not
        # be the hard-cap path.
        assert not result.get("hard_cap"), (
            "wall fired one block early"
        )


class TestDeepDrainRamp:
    def test_drain_at_start_depth_matches_base(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """At exactly LUMINOSITY_DEEP_DRAIN_START_DEPTH, the bonus is 0 —
        drain equals the base layer rate."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_player(
            dig_service, dig_repo, player_repository,
            depth=LUMINOSITY_DEEP_DRAIN_START_DEPTH,
        )
        random.seed(99)
        result = dig_service.dig(10001, 12345)
        assert result.get("success"), f"dig failed: {result.get('error')}"
        lum_info = result.get("luminosity_info") or {}
        # The Hollow base drain is 10
        assert lum_info.get("drained") == LUMINOSITY_DRAIN_PER_DIG["The Hollow"]

    def test_drain_at_cap_is_double(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Right at depth cap the drain bonus is +10, doubling the base
        Hollow drain. Test at depth cap-1 since exactly cap is rejected."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_player(
            dig_service, dig_repo, player_repository,
            depth=PRESTIGE_HARD_CAP - 1,
        )
        random.seed(99)
        result = dig_service.dig(10001, 12345)
        assert result.get("success"), f"dig failed: {result.get('error')}"
        lum_info = result.get("luminosity_info") or {}
        base = LUMINOSITY_DRAIN_PER_DIG["The Hollow"]
        # depth-1 = 499, expected bonus = (499-300)//20 = 9; total = 19
        # (depth ramp uses depth_before, captured before the dig advance)
        expected_bonus = (PRESTIGE_HARD_CAP - 1 - LUMINOSITY_DEEP_DRAIN_START_DEPTH) // (
            LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP
        )
        assert lum_info.get("drained") == base + expected_bonus

    def test_drain_below_start_depth_unchanged(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Below the start depth there's no bonus — drain matches the
        layer's base rate."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_player(
            dig_service, dig_repo, player_repository,
            depth=LUMINOSITY_DEEP_DRAIN_START_DEPTH - 50,
        )
        random.seed(99)
        result = dig_service.dig(10001, 12345)
        assert result.get("success"), f"dig failed: {result.get('error')}"
        lum_info = result.get("luminosity_info") or {}
        # depth 250 is in Frozen Core (201-275) per the layer table
        assert lum_info.get("drained") == LUMINOSITY_DRAIN_PER_DIG["Frozen Core"]

    def test_drain_increases_monotonically(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Drain at depth 350 < drain at 400 < drain at 450 (all in The
        Hollow with progressively larger bonuses)."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        # Register once, then update_tunnel to reposition between digs.
        _seed_player(dig_service, dig_repo, player_repository, depth=350)
        drains = []
        for d in (350, 400, 450):
            dig_repo.update_tunnel(
                10001, 12345,
                depth=d, luminosity=100,
                last_dig_at=0, last_lum_update_at=0,
            )
            random.seed(99)
            result = dig_service.dig(10001, 12345)
            assert result.get("success"), f"dig at {d} failed: {result.get('error')}"
            drains.append(result["luminosity_info"]["drained"])
        assert drains[0] < drains[1] < drains[2], drains


class TestPostPinnacleDecay:
    """Per-dig JC and artifact rate fall off past the pinnacle so
    grinders see diminishing returns rather than free farm."""

    def test_factor_at_or_below_pinnacle_is_one(self, dig_service):
        from services.dig_constants import PINNACLE_DEPTH

        assert dig_service._post_pinnacle_decay_factor(0) == 1.0
        assert dig_service._post_pinnacle_decay_factor(PINNACLE_DEPTH - 1) == 1.0
        assert dig_service._post_pinnacle_decay_factor(PINNACLE_DEPTH) == 1.0

    def test_factor_steps_down_per_25_depth(self, dig_service):
        from services.dig_constants import PINNACLE_DEPTH

        # First step at depth 325: -5%
        assert dig_service._post_pinnacle_decay_factor(PINNACLE_DEPTH + 25) == pytest.approx(0.95)
        # Depth 400: 4 steps × 5% = 20% off → 0.80
        assert dig_service._post_pinnacle_decay_factor(PINNACLE_DEPTH + 100) == pytest.approx(0.80)
        # Depth 500: 8 steps × 5% = 40% off → 0.60
        assert dig_service._post_pinnacle_decay_factor(PINNACLE_DEPTH + 200) == pytest.approx(0.60)

    def test_factor_clamps_at_zero(self, dig_service):
        from services.dig_constants import PINNACLE_DEPTH

        # At -5% per 25 depth, factor hits 0 at PINNACLE + 500 (depth 800)
        # which is well past PRESTIGE_HARD_CAP, so we test a hypothetical
        # to confirm the clamp.
        assert dig_service._post_pinnacle_decay_factor(PINNACLE_DEPTH + 1000) == 0.0

    def test_partial_steps_round_down(self, dig_service):
        from services.dig_constants import PINNACLE_DEPTH

        # 24 depth past pinnacle: still 0 steps → factor 1.0
        assert dig_service._post_pinnacle_decay_factor(PINNACLE_DEPTH + 24) == 1.0
        # 26 depth past: 1 step → factor 0.95
        assert dig_service._post_pinnacle_decay_factor(PINNACLE_DEPTH + 26) == pytest.approx(0.95)
