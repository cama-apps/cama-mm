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

import services.dig_service as dig_service_module
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


def _threat_event(event_id: str, failure_outcome: dict) -> dict:
    """A two-branch event whose risky FAILURE carries the given outcome.

    Used to drive a jitter check against a streak_loss / curse payload —
    the failure outcome is what the threat code reads.
    """
    return {
        "id": event_id,
        "name": f"Synthetic {event_id}",
        "description": ("A test passage.",),
        "min_depth": 0,
        "max_depth": None,
        "safe_option": {
            "label": "Safe",
            "success": {"description": "Safe.", "advance": 0, "jc": 2,
                        "cave_in": False, "streak_loss": 0, "curse": None},
            "failure": None,
            "success_chance": 1.0,
        },
        "risky_option": {
            "label": "Risky",
            "success": {"description": "Risky win.", "advance": 0, "jc": 20,
                        "cave_in": False, "streak_loss": 0, "curse": None},
            "failure": failure_outcome,
            "success_chance": 0.5,
        },
        "complexity": "choice",
        "layer": None,
        "rarity": "common",
        "requires_dark": False,
        "social": False,
        "ascii_art": None,
        "buff_on_success": None,
        "desperate_option": None,
        "boon_options": None,
        "min_prestige": 0,
        "next_event_id": None,
        "chain_only": False,
        "splash": None,
        "guild_modifier_on_success": None,
        "quest_id": None,
        "quest_step": None,
    }


@pytest.fixture
def inject_event():
    """Append a synthetic event to EVENT_POOL and tear it down after."""
    added: list[str] = []

    def _add(event: dict) -> dict:
        dig_service_module.EVENT_POOL.append(event)
        added.append(event["id"])
        return event

    yield _add

    dig_service_module.EVENT_POOL[:] = [
        e for e in dig_service_module.EVENT_POOL if e["id"] not in added
    ]


class TestEventVariance:
    def test_jc_jitter_covers_expected_range_and_mean(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """200 resolutions of underground_stream safe (jc=3) should land
        within [2, 4] and the empirical mean should be near 3."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_tunnel(dig_service, dig_repo, player_repository)

        rolls = []
        for i in range(200):
            random.seed(i + 1)
            r = dig_service.resolve_event(10001, 12345, "underground_stream", "safe")
            assert r["success"]
            rolls.append(r.get("jc_delta", 0))

        assert min(rolls) >= 2  # int(round(3 * 0.5)) = 2
        assert max(rolls) <= 4  # int(round(3 * 1.5)) = 4
        mean = sum(rolls) / len(rolls)
        assert 2.5 <= mean <= 3.5, f"mean {mean} not within ±15% of base 3"
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

    def test_negative_jc_stays_negative(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """End-to-end: a failure outcome carrying a negative base JC must
        always credit the digger a *negative* jc_delta — the ±50% jitter
        (and the NEGATIVE_EVENT_JC_MULTIPLIER applied before it) preserve the
        sign of a loss. Drives the real ``resolve_event`` jitter path so a
        regression that floored or flipped negative payouts is caught."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_tunnel(dig_service, dig_repo, player_repository)
        inject_event(_threat_event(
            "variance_neg_jc",
            {"description": "Robbed.", "advance": 0, "jc": -10,
             "cave_in": False, "streak_loss": 0, "curse": None},
        ))

        deltas = set()
        for i in range(60):
            random.seed(i + 1)
            dig_repo.update_tunnel(10001, 12345, depth=30)
            # Force the risky pick to FAIL so the negative-jc outcome fires.
            monkeypatch.setattr(
                "services.dig.events_mixin.random.random", lambda: 0.99,
            )
            r = dig_service.resolve_event(10001, 12345, "variance_neg_jc", "risky")
            assert r["success"]
            deltas.add(r.get("jc_delta"))
        assert deltas, "no resolutions ran"
        assert all(d < 0 for d in deltas), (
            f"a negative-base outcome credited a non-negative jc_delta: "
            f"{sorted(deltas)}"
        )
        assert len(deltas) >= 3, (
            f"jitter not firing on the negative payout — only {len(deltas)} "
            "unique deltas"
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


class TestThreatPayloadUnjittered:
    """The outcome jitter scales JC and shifts advance only. A streak_loss or
    curse on a failure outcome must pass through untouched — jitter must not
    corrupt a threat payload."""

    def test_streak_loss_payload_not_jittered(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """A failure outcome with streak_loss=3, resolved across 60 seeds,
        always reports streak_loss=3 — the jitter never scales it. With a
        fixed 10-day streak, setback = 3 + floor(10/20) = 3 every time."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_tunnel(dig_service, dig_repo, player_repository)
        inject_event(_threat_event(
            "variance_streak",
            {"description": "Momentum lost.", "advance": 0, "jc": 0,
             "cave_in": False, "streak_loss": 3, "curse": None},
        ))

        seen = set()
        for i in range(60):
            random.seed(i + 1)
            dig_repo.update_tunnel(10001, 12345, streak_days=10)
            # Force the risky pick to FAIL so the streak_loss outcome fires.
            monkeypatch.setattr(
                "services.dig.events_mixin.random.random", lambda: 0.99,
            )
            r = dig_service.resolve_event(10001, 12345, "variance_streak", "risky")
            assert r["success"]
            seen.add(r.get("streak_loss"))
        assert seen == {3}, f"streak_loss was jittered — saw {sorted(seen)}"

    def test_curse_payload_not_jittered(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """A failure outcome carrying a curse (duration_digs=2) resolved
        across 60 seeds always applies the same curse — the curse payload is
        strengthened deterministically (fixed +1 duration / scaled effect) and
        never touched by the per-fire JC jitter."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        _seed_tunnel(dig_service, dig_repo, player_repository)
        curse = {
            "id": "variance_hex", "name": "Variance Hex",
            "duration_digs": 2, "effect": {"advance_bonus": -3},
        }
        inject_event(_threat_event(
            "variance_curse",
            {"description": "Hexed.", "advance": 0, "jc": 0,
             "cave_in": False, "streak_loss": 0, "curse": curse},
        ))

        durations = set()
        for i in range(60):
            random.seed(i + 1)
            # Clear any curse left by the prior iteration's dig.
            dig_repo.update_tunnel(10001, 12345, temp_curses=None)
            monkeypatch.setattr(
                "services.dig.events_mixin.random.random", lambda: 0.99,
            )
            r = dig_service.resolve_event(10001, 12345, "variance_curse", "risky")
            assert r["success"]
            applied = r.get("curse_applied")
            assert applied is not None and applied["id"] == "variance_hex"
            tunnel = dig_repo.get_tunnel(10001, 12345)
            active = dig_service._get_active_curse(dict(tunnel))
            assert active is not None
            # Deterministic curse strengthening (not jitter): -3 -> -4, dur 2 -> 3.
            assert active["effect"] == {"advance_bonus": -4}, "curse effect jittered"
            durations.add(active["digs_remaining"])
        assert durations == {3}, f"curse duration was jittered — saw {sorted(durations)}"
