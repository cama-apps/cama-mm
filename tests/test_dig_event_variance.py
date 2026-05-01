"""Outcome jitter on EventChoice resolution: ±50% JC, ±2 advance with
sign-preserving clamp. Variance is invisible to players — these tests
exercise the rolled values directly off the service result.

Strategy: drive variance through *safe* options (success_chance=1.0)
so the success/failure roll never branches and only the outcome jitter
varies between resolutions. Each iteration reseeds random for a fresh
trajectory through the jitter rolls.
"""

from __future__ import annotations

import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _seed_tunnel(dig_service, dig_repo, player_repository, depth: int = 30):
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
    dig_repo.update_tunnel(10001, 12345, depth=depth, luminosity=100)


class TestEventVariance:
    def test_jc_jitter_covers_expected_range_and_mean(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """200 resolutions of underground_stream safe (jc=4) should land
        within [2, 6] and the empirical mean should be near 4."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_tunnel(dig_service, dig_repo, player_repository)

        rolls = []
        for i in range(200):
            random.seed(i + 1)
            r = dig_service.resolve_event(10001, 12345, "underground_stream", "safe")
            assert r["success"]
            rolls.append(r.get("jc_delta", 0))

        assert min(rolls) >= 2  # int(round(4 * 0.5)) = 2
        assert max(rolls) <= 6  # int(round(4 * 1.5)) = 6
        mean = sum(rolls) / len(rolls)
        assert 3.5 <= mean <= 4.5, f"mean {mean} not within ±15% of base 4"
        assert len({*rolls}) >= 3, "no jitter visible across 200 rolls"

    def test_zero_jc_outcome_stays_zero(self):
        """A base outcome of 0 JC should never roll non-zero (rounding
        artifact protection)."""
        for x in (0.5, 1.0, 1.5, 0.73, 1.42):
            assert int(round(0 * x)) == 0

    def test_zero_advance_outcome_stays_zero(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """End-to-end: an event whose safe option has advance=0 must
        never produce depth_delta != 0 after jitter. Guards the
        ``if advance != 0`` skip in resolve_event."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_tunnel(dig_service, dig_repo, player_repository)

        for i in range(80):
            random.seed(i + 700)
            dig_repo.update_tunnel(10001, 12345, depth=30)
            r = dig_service.resolve_event(10001, 12345, "underground_stream", "safe")
            assert r["success"]
            assert r.get("depth_delta", 0) == 0, (
                f"advance jitter fired on a 0-base outcome (iter {i}, "
                f"got {r.get('depth_delta')})"
            )

    def test_advance_sign_clamp_no_retreat_from_success(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """friendly_mole safe success has advance=+2. With ±2 jitter the
        raw range is [0, 4]; sign clamp must lift any non-positive roll
        to +1 so a successful event never reverses into a retreat."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_tunnel(dig_service, dig_repo, player_repository, depth=10)

        for i in range(80):
            random.seed(i + 100)
            # Reset depth so boss-boundary clamp doesn't wipe advance to 0
            dig_repo.update_tunnel(10001, 12345, depth=10)
            r = dig_service.resolve_event(10001, 12345, "friendly_mole", "safe")
            assert r["success"]
            assert r.get("depth_delta", 0) >= 1, (
                f"advance {r.get('depth_delta')} on iter {i} retreated from "
                "a positive-base success"
            )

    def test_negative_jc_stays_negative(self):
        """A negative JC base must roll negative — int(round(neg * x))
        preserves sign for any x in [0.5, 1.5]."""
        for base in (-1, -5, -15, -100):
            for _ in range(50):
                rolled = int(round(base * random.uniform(0.5, 1.5)))
                assert rolled < 0, (
                    f"base {base} rolled to {rolled} (positive after jitter)"
                )

    def test_jitter_actually_varies_across_seeds(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Sanity: 50 seeded resolutions should not all return the same
        jc_delta (proves jitter is wired in)."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_tunnel(dig_service, dig_repo, player_repository)

        rolls = set()
        for i in range(50):
            random.seed(i + 500)
            r = dig_service.resolve_event(10001, 12345, "underground_stream", "safe")
            rolls.add(r.get("jc_delta", 0))
        assert len(rolls) >= 3, f"variance not firing — only {len(rolls)} unique rolls"
