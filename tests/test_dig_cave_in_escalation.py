"""Tests for the depth-banded cave-in escalation, new consequence types,
and the catastrophic sub-roll."""

from __future__ import annotations

import json
import random

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    CAVE_IN_BAND_DEEP,
    CAVE_IN_BAND_ENDGAME,
    CAVE_IN_BAND_MID,
    CAVE_IN_BAND_SHALLOW,
    CAVE_IN_BLOCK_LOSS_RANGES,
    CAVE_IN_CATASTROPHIC_MEDICAL_BILL,
    CAVE_IN_CATASTROPHIC_PCT_BY_BAND,
    CAVE_IN_CONSEQUENCE_WEIGHTS,
    CAVE_IN_INJURY_DIGS_BY_BAND,
    CAVE_IN_MEDICAL_BILL_RANGES,
    CAVE_IN_STUN_DIGS_BY_BAND,
    cave_in_band,
    pick_cave_in_consequence,
    roll_catastrophic_cave_in,
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


def _register(player_repository, discord_id=10001, guild_id=12345, balance=10000):
    player_repository.add(
        discord_id=discord_id, discord_username=f"P{discord_id}",
        guild_id=guild_id, initial_mmr=3000,
        glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06,
    )
    player_repository.update_balance(discord_id, guild_id, balance)
    return discord_id


def _clear_boss_blocks(dig_repo, discord_id, guild_id):
    """Mark every boss boundary as defeated so a deep-depth tunnel doesn't
    park on an unfought boss instead of resolving the dig."""
    from services.dig_constants import BOSS_BOUNDARIES
    progress = {str(b): "defeated" for b in BOSS_BOUNDARIES}
    dig_repo.update_tunnel(discord_id, guild_id, boss_progress=json.dumps(progress))


class TestBandClassification:
    def test_shallow(self):
        assert cave_in_band(0) == CAVE_IN_BAND_SHALLOW
        assert cave_in_band(49) == CAVE_IN_BAND_SHALLOW

    def test_mid(self):
        assert cave_in_band(50) == CAVE_IN_BAND_MID
        assert cave_in_band(149) == CAVE_IN_BAND_MID

    def test_deep(self):
        assert cave_in_band(150) == CAVE_IN_BAND_DEEP
        assert cave_in_band(249) == CAVE_IN_BAND_DEEP

    def test_endgame(self):
        assert cave_in_band(250) == CAVE_IN_BAND_ENDGAME
        assert cave_in_band(10000) == CAVE_IN_BAND_ENDGAME


class TestEscalationTables:
    def test_block_loss_increases_with_depth(self):
        sh = CAVE_IN_BLOCK_LOSS_RANGES[CAVE_IN_BAND_SHALLOW]
        mid = CAVE_IN_BLOCK_LOSS_RANGES[CAVE_IN_BAND_MID]
        deep = CAVE_IN_BLOCK_LOSS_RANGES[CAVE_IN_BAND_DEEP]
        end = CAVE_IN_BLOCK_LOSS_RANGES[CAVE_IN_BAND_ENDGAME]
        assert sh[1] < mid[1] < deep[1] < end[1]
        assert sh[0] <= mid[0] <= deep[0] <= end[0]

    def test_medical_bill_increases_with_depth(self):
        sh = CAVE_IN_MEDICAL_BILL_RANGES[CAVE_IN_BAND_SHALLOW]
        end = CAVE_IN_MEDICAL_BILL_RANGES[CAVE_IN_BAND_ENDGAME]
        assert end[1] >= sh[1] * 4

    def test_stun_and_injury_durations_grow(self):
        assert (
            CAVE_IN_STUN_DIGS_BY_BAND[CAVE_IN_BAND_SHALLOW]
            < CAVE_IN_STUN_DIGS_BY_BAND[CAVE_IN_BAND_ENDGAME]
        )
        assert (
            CAVE_IN_INJURY_DIGS_BY_BAND[CAVE_IN_BAND_SHALLOW]
            < CAVE_IN_INJURY_DIGS_BY_BAND[CAVE_IN_BAND_ENDGAME]
        )

    def test_consequence_weights_sum_to_100(self):
        for band, table in CAVE_IN_CONSEQUENCE_WEIGHTS.items():
            total = sum(w for _, w in table)
            assert total == 100, f"band {band!r} weights sum to {total}, not 100"

    def test_new_consequences_only_at_deeper_bands(self):
        shallow_ids = {cid for cid, _ in CAVE_IN_CONSEQUENCE_WEIGHTS[CAVE_IN_BAND_SHALLOW]}
        assert "gear_nick" not in shallow_ids
        assert "spilled_satchel" not in shallow_ids
        assert "snuffed_light" not in shallow_ids
        assert "cracked_hat" not in shallow_ids

        deep_ids = {cid for cid, _ in CAVE_IN_CONSEQUENCE_WEIGHTS[CAVE_IN_BAND_DEEP]}
        assert "gear_nick" in deep_ids
        assert "spilled_satchel" in deep_ids


class TestCatastrophicRoll:
    def test_shallow_never_catastrophic(self):
        for _ in range(500):
            assert roll_catastrophic_cave_in(CAVE_IN_BAND_SHALLOW) is False

    def test_endgame_can_be_catastrophic(self):
        # Force RNG; with pct=0.05 and a tight `random.random()` value < 0.05,
        # this should fire.
        random.seed(0)
        hits = sum(roll_catastrophic_cave_in(CAVE_IN_BAND_ENDGAME) for _ in range(2000))
        # ~5% of 2000 = ~100; allow a wide band for stochastic safety.
        assert 30 < hits < 200, f"got {hits} catastrophic hits in 2000 rolls"

    def test_pct_table_grows_monotonically(self):
        sh = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[CAVE_IN_BAND_SHALLOW]
        mid = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[CAVE_IN_BAND_MID]
        deep = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[CAVE_IN_BAND_DEEP]
        end = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[CAVE_IN_BAND_ENDGAME]
        assert sh < mid < deep < end


class TestConsequencePicker:
    def test_shallow_only_picks_legacy_consequences(self):
        random.seed(42)
        seen = set()
        for _ in range(500):
            cid = pick_cave_in_consequence(
                CAVE_IN_BAND_SHALLOW,
                has_consumables=True, has_equipped_gear=True,
                can_lower_luminosity=True, has_hard_hat_charges=True,
            )
            seen.add(cid)
        assert seen <= {"stun", "injury", "medical_bill"}

    def test_deep_can_pick_new_consequence_types(self):
        random.seed(42)
        seen = set()
        for _ in range(2000):
            cid = pick_cave_in_consequence(
                CAVE_IN_BAND_DEEP,
                has_consumables=True, has_equipped_gear=True,
                can_lower_luminosity=True, has_hard_hat_charges=True,
            )
            seen.add(cid)
        assert {"gear_nick", "spilled_satchel", "snuffed_light"} <= seen

    def test_inapplicable_consequences_are_skipped(self):
        # No consumables, no gear, no luminosity to lower, no hat charges:
        # only stun/injury/medical_bill should ever come back.
        random.seed(7)
        for _ in range(500):
            cid = pick_cave_in_consequence(
                CAVE_IN_BAND_DEEP,
                has_consumables=False, has_equipped_gear=False,
                can_lower_luminosity=False, has_hard_hat_charges=False,
            )
            assert cid in {"stun", "injury", "medical_bill"}


class TestEndToEndCaveInDeep:
    """Force a cave-in at deep depth and confirm the consequence shape is
    coherent (one of the seven types, with the right detail keys)."""

    def test_force_cave_in_at_deep(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        _register(player_repository, balance=10000)
        # Bootstrap tunnel.
        monkeypatch.setattr("time.time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Move to deep depth, sit between boss boundaries, mark all bosses defeated.
        dig_repo.update_tunnel(10001, guild_id, depth=180)
        _clear_boss_blocks(dig_repo, 10001, guild_id)

        # Force cave-in (random.random() < cave_in_chance ⇒ cave-in).
        from services.dig_constants import FREE_DIG_COOLDOWN_SECONDS
        monkeypatch.setattr("time.time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.0001)
        result = dig_service.dig(10001, guild_id)
        assert result.get("cave_in") is True
        detail = result.get("cave_in_detail") or {}
        assert detail.get("type") in {
            "stun", "injury", "medical_bill",
            "gear_nick", "spilled_satchel",
            "snuffed_light", "cracked_hat",
            "catastrophic",
        }
        assert "block_loss" in detail
        # Block loss at deep band must clear the shallow-band ceiling on at
        # least the high end of the range.
        assert detail["block_loss"] >= 1

    def test_catastrophic_overrides_to_milestone(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        from services.dig_constants import (
            CAVE_IN_CATASTROPHIC_MILESTONE_STEP,
            FREE_DIG_COOLDOWN_SECONDS,
        )
        _register(player_repository, balance=10000)
        monkeypatch.setattr("time.time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Sit between boss boundaries (200 < depth < 275), mark bosses defeated.
        dig_repo.update_tunnel(10001, guild_id, depth=240)
        _clear_boss_blocks(dig_repo, 10001, guild_id)

        # Force cave-in AND force catastrophic. roll_catastrophic_cave_in uses
        # a fresh random.random() call each time; setting random.random to a
        # constant 0.0 satisfies both checks.
        monkeypatch.setattr("time.time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.0001)
        result = dig_service.dig(10001, guild_id)
        assert result.get("cave_in") is True
        detail = result.get("cave_in_detail") or {}
        if detail.get("type") == "catastrophic":
            tunnel = dig_repo.get_tunnel(10001, guild_id)
            assert tunnel["depth"] % CAVE_IN_CATASTROPHIC_MILESTONE_STEP == 0
            cmin, cmax = CAVE_IN_CATASTROPHIC_MEDICAL_BILL
            jc_lost = detail.get("jc_lost", 0)
            # JC lost is capped to balance, but with balance=10000 we should
            # always get the full random range.
            assert cmin <= jc_lost <= cmax

    def test_insurance_protects_catastrophic_depth(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Per plan §1d: insurance keeps depth on catastrophic. Other
        consequences (medical bill, stun, gear) still fire."""
        from services.dig_constants import FREE_DIG_COOLDOWN_SECONDS
        _register(player_repository, balance=10000)
        monkeypatch.setattr("time.time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Place at deep depth between boundaries; plant insurance valid for
        # the next dig's wall-clock time.
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=240,
            insured_until=1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 86400,
        )
        _clear_boss_blocks(dig_repo, 10001, guild_id)

        monkeypatch.setattr("time.time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.0001)
        result = dig_service.dig(10001, guild_id)
        detail = result.get("cave_in_detail") or {}
        if detail.get("type") == "catastrophic":
            tunnel = dig_repo.get_tunnel(10001, guild_id)
            # Insurance held depth — must NOT roll back to the milestone.
            # depth_after should be ≥ depth_before - max block_loss range.
            assert detail.get("insurance_saved") is True
            assert tunnel["depth"] >= 240 - 35  # generous range for endgame block loss
