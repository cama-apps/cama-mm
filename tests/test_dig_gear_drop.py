"""Tests for the boss-drop helper (_maybe_drop_gear).

The drop rolls are RNG-driven; we use ``random.seed`` for determinism so the
hit-rate test isn't flaky. The seed is restored automatically by the
``_isolate_random_state`` autouse fixture in conftest.
"""

import random

import pytest

from domain.models.dig_gear import GearSlot
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_constants import (
    GEAR_BOSS_DROP_RATE,
    GEAR_DROP_DEPTH_TIER_MAP,
    GEAR_TIER_TABLES,
)
from services.dig_service import DigService


@pytest.fixture
def svc(repo_db_path):
    drepo = DigRepository(repo_db_path)
    prepo = PlayerRepository(repo_db_path)
    s = DigService(drepo, prepo)
    s.player_repo.add(discord_id=111, discord_username="pf", guild_id=0)
    s.dig_repo.create_tunnel(111, 0, "Test")
    return s


class TestDropGearGate:
    def test_returns_none_for_unmapped_boundary(self, svc):
        """Bosses outside GEAR_DROP_DEPTH_TIER_MAP never drop gear."""
        random.seed(0)
        # Try 100 rolls at boundary 25 (not in the map) — should never drop
        for _ in range(100):
            assert svc._maybe_drop_gear(111, 0, 25) is None

    def test_returns_none_when_roll_misses(self, svc, monkeypatch):
        """If random.random() returns >= GEAR_BOSS_DROP_RATE, no drop."""
        monkeypatch.setattr(random, "random", lambda: GEAR_BOSS_DROP_RATE + 0.01)
        assert svc._maybe_drop_gear(111, 0, 100) is None


class TestDropTierMatchesDepth:
    def test_each_mapped_boundary_drops_correct_tier(self, svc, monkeypatch):
        """Force a hit and confirm the dropped tier matches the boundary map."""
        # Pin random.random to 0 so the gate always passes; let random.choice
        # use real RNG for slot picking.
        monkeypatch.setattr(random, "random", lambda: 0.0)
        for boundary, expected_tier in GEAR_DROP_DEPTH_TIER_MAP.items():
            drop = svc._maybe_drop_gear(111, 0, boundary)
            assert drop is not None
            assert drop["tier"] == expected_tier
            assert drop["slot"] in {"weapon", "armor", "boots"}
            # Resolve the slot/tier through the GEAR_TIER_TABLES map and
            # confirm the returned name matches the canonical entry.
            slot_enum = GearSlot(drop["slot"])
            expected_name = GEAR_TIER_TABLES[slot_enum][expected_tier].name
            assert drop["name"] == expected_name


class TestDropPersists:
    def test_drop_creates_dig_gear_row(self, svc, monkeypatch):
        monkeypatch.setattr(random, "random", lambda: 0.0)
        monkeypatch.setattr(random, "choice", lambda choices: "armor")
        drop = svc._maybe_drop_gear(111, 0, 100)
        assert drop is not None
        owned = svc.dig_repo.get_gear(111, 0)
        assert any(
            g["id"] == drop["gear_id"]
            and g["slot"] == "armor"
            and g["source"] == "boss_drop"
            for g in owned
        )


class TestDropRate:
    """Statistical sanity check on the drop rate via seeded RNG."""

    def test_rate_in_band_over_5000_rolls(self, svc):
        """Over 5000 seeded rolls the empirical rate should sit in
        [GEAR_BOSS_DROP_RATE - 0.02, GEAR_BOSS_DROP_RATE + 0.02].

        With p=0.07 and n=5000 the binomial std-dev is ~0.0036 — a 2pp band
        is well over 5σ in either direction so this is robust to seed drift.
        """
        random.seed(42)
        hits = 0
        n = 5000
        for _ in range(n):
            if svc._maybe_drop_gear(111, 0, 100) is not None:
                hits += 1
        rate = hits / n
        assert abs(rate - GEAR_BOSS_DROP_RATE) < 0.02, (
            f"observed rate {rate:.4f} drifted from target {GEAR_BOSS_DROP_RATE:.4f}"
        )
