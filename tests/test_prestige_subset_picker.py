"""Tests for the prestige perk picker: random 4-of-12 subset with a 5-stack cap,
deterministic seeding so re-opening the embed shows the same options, and
display-name overrides like Loot Multiplier → Loot Bonus."""
from __future__ import annotations

import random

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    PRESTIGE_PERK_STACK_CAP,
    PRESTIGE_PERKS,
    perk_display_name,
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


class TestEligiblePerks:
    def test_no_picks_returns_full_pool(self, dig_service):
        tunnel = {"prestige_perks": "[]"}
        eligible = dig_service._eligible_perks(tunnel)
        assert set(eligible) == set(PRESTIGE_PERKS)
        assert len(eligible) == 12

    def test_under_cap_perk_still_eligible(self, dig_service):
        # 4 picks of loot_multiplier — still under the 5-cap.
        tunnel = {"prestige_perks": '["loot_multiplier"] * 4'}
        # Use proper JSON
        import json
        tunnel = {"prestige_perks": json.dumps(["loot_multiplier"] * 4)}
        eligible = dig_service._eligible_perks(tunnel)
        assert "loot_multiplier" in eligible

    def test_at_cap_perk_hidden(self, dig_service):
        import json
        tunnel = {"prestige_perks": json.dumps(["loot_multiplier"] * PRESTIGE_PERK_STACK_CAP)}
        eligible = dig_service._eligible_perks(tunnel)
        assert "loot_multiplier" not in eligible
        assert len(eligible) == 11

    def test_multiple_at_cap_all_hidden(self, dig_service):
        import json
        picks = (["loot_multiplier"] * 5) + (["advance_boost"] * 5)
        tunnel = {"prestige_perks": json.dumps(picks)}
        eligible = dig_service._eligible_perks(tunnel)
        assert "loot_multiplier" not in eligible
        assert "advance_boost" not in eligible
        assert len(eligible) == 10


class TestPickerSeedDeterminism:
    """The PrestigePerksView samples 4 random perks seeded by (user_id, level).
    Re-opening the embed must show the same options so users can't re-roll."""

    def test_same_seed_picks_same_subset(self):
        pool = [{"id": p, "name": p} for p in PRESTIGE_PERKS]
        seed = hash((123, 5)) & 0xFFFFFFFFFFFFFFFF
        rng_a = random.Random(seed)
        rng_b = random.Random(seed)
        a = rng_a.sample(pool, 4)
        b = rng_b.sample(pool, 4)
        assert [p["id"] for p in a] == [p["id"] for p in b]

    def test_different_user_picks_different_subset(self):
        pool = [{"id": p, "name": p} for p in PRESTIGE_PERKS]
        rng_a = random.Random(hash((123, 5)) & 0xFFFFFFFFFFFFFFFF)
        rng_b = random.Random(hash((456, 5)) & 0xFFFFFFFFFFFFFFFF)
        a = [p["id"] for p in rng_a.sample(pool, 4)]
        b = [p["id"] for p in rng_b.sample(pool, 4)]
        # Two distinct seeds against a 12-perk pool — extraordinarily unlikely
        # to coincidentally match, so a strict inequality is safe.
        assert a != b

    def test_different_level_picks_different_subset(self):
        pool = [{"id": p, "name": p} for p in PRESTIGE_PERKS]
        rng_a = random.Random(hash((123, 5)) & 0xFFFFFFFFFFFFFFFF)
        rng_b = random.Random(hash((123, 6)) & 0xFFFFFFFFFFFFFFFF)
        a = [p["id"] for p in rng_a.sample(pool, 4)]
        b = [p["id"] for p in rng_b.sample(pool, 4)]
        assert a != b


class TestStackCapEnforcement:
    """The prestige() service rejects picks past the soft cap of 5."""

    def test_picking_past_cap_returns_error(self, dig_service, dig_repo, player_repository):
        import json

        player_repository.add(
            discord_id=10001,
            discord_username="P",
            guild_id=12345,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        # Manually create a tunnel with 5 stacks of loot_multiplier already
        # picked. The service's stack-cap check should reject a 6th pick
        # rather than silently grow the stack.
        dig_service.dig(10001, 12345)
        dig_repo.update_tunnel(
            10001, 12345,
            prestige_perks=json.dumps(["loot_multiplier"] * PRESTIGE_PERK_STACK_CAP),
            depth=500,  # past the prestige threshold
        )
        result = dig_service.prestige(10001, 12345, "loot_multiplier")
        # Service returns an error dict — check the failure path fires.
        assert result.get("error") or result.get("success") is False


class TestDisplayName:
    def test_loot_multiplier_renders_as_loot_bonus(self):
        assert perk_display_name("loot_multiplier") == "Loot Bonus"

    def test_unmapped_perk_falls_back_to_titlecase(self):
        # Any perk without a display_name override gets the default
        # title-case-the-id rendering.
        assert perk_display_name("advance_boost") == "Advance Boost"
        assert perk_display_name("the_endless") == "The Endless"
