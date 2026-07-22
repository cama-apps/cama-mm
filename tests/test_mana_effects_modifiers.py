"""Tests for new mana_effects modifier methods (loan/sabotage/boss/weather)."""

from __future__ import annotations

import pytest

from domain.models.mana_effects import ManaEffects

# ──────────────────────────────────────────────────────────────────
# ManaEffects dataclass: per-color new-field defaults
# ──────────────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "color,land,expected_loan_fee,expected_loan_limit_mult",
    [
        ("White", "Plains", 0.5, 1.0),
        ("Black", "Swamp", 1.5, 2.0),
        ("Red", "Mountain", 1.0, 1.0),
        ("Blue", "Island", 1.0, 1.0),
        ("Green", "Forest", 1.0, 1.0),
    ],
)
def test_loan_modifiers_per_color(color, land, expected_loan_fee, expected_loan_limit_mult):
    effects = ManaEffects.for_color(color, land)
    assert effects.loan_fee_mult == expected_loan_fee
    assert effects.loan_limit_mult == expected_loan_limit_mult


def test_sabotage_modifiers_per_color():
    red = ManaEffects.for_color("Red", "Mountain")
    black = ManaEffects.for_color("Black", "Swamp")
    white = ManaEffects.for_color("White", "Plains")
    blue = ManaEffects.for_color("Blue", "Island")
    green = ManaEffects.for_color("Green", "Forest")

    assert red.sabotage_cost_mult == 0.5
    assert black.sabotage_steal_depth_pct == 0.25
    assert white.sabotage_first_aegis_today is False
    assert white.plains_guardian_aura is True
    assert blue.sabotage_reveal_attacker is True
    assert green.sabotage_passive_recovery is True


def test_boss_combat_modifiers_per_color():
    white = ManaEffects.for_color("White", "Plains")
    black = ManaEffects.for_color("Black", "Swamp")
    red = ManaEffects.for_color("Red", "Mountain")
    blue = ManaEffects.for_color("Blue", "Island")
    green = ManaEffects.for_color("Green", "Forest")

    assert white.boss_damage_mult == 1.20
    assert black.boss_hp_mult == 1.30
    assert black.boss_loot_mult == 1.25
    assert red.boss_dynamite_damage_mult == 1.5
    assert blue.boss_reveal_hp is True
    assert green.boss_no_crit_against is True


def test_weather_combo_per_color():
    blue = ManaEffects.for_color("Blue", "Island")
    white = ManaEffects.for_color("White", "Plains")
    black = ManaEffects.for_color("Black", "Swamp")
    red = ManaEffects.for_color("Red", "Mountain")
    green = ManaEffects.for_color("Green", "Forest")

    assert blue.weather_combo_storm_cooldown_mult == 0.5
    assert white.weather_combo_sunny_yield_mult == 1.10
    assert black.weather_combo_fog_hazard_mult == 0.5
    assert red.weather_combo_heat_dynamite_extra_chains == 2
    assert green.weather_combo_rain_recovery_mult == 2.0


def test_no_color_is_all_defaults():
    """Empty / no-color ManaEffects has no modifiers."""
    e = ManaEffects()
    assert e.loan_fee_mult == 1.0
    assert e.sabotage_cost_mult == 1.0
    assert e.boss_damage_mult == 1.0
    assert e.weather_combo_storm_cooldown_mult == 1.0


# ──────────────────────────────────────────────────────────────────
# ManaEffectsService modifier methods (loan/sabotage/boss/weather)
# ──────────────────────────────────────────────────────────────────


class _StubManaService:
    def __init__(self, color: str | None, land: str | None = None, consumed: bool = False):
        self._color = color
        self._land = land
        self._consumed = consumed

    def get_current_mana(self, discord_id, guild_id):
        if self._color is None:
            return None
        from services.mana_service import get_today_pst
        return {
            "land": self._land,
            "color": self._color,
            "emoji": "?",
            "assigned_date": get_today_pst(),
            "consumed": self._consumed,
        }

    def is_mana_consumed(self, discord_id, guild_id):
        return self._consumed


@pytest.fixture
def effects_service_factory():
    """Build a ManaEffectsService configured around a stub mana_service."""
    from services.mana_effects_service import ManaEffectsService

    def _make(color, land=None, consumed=False):
        stub = _StubManaService(color=color, land=land, consumed=consumed)
        return ManaEffectsService(
            mana_service=stub,
            player_repo=None,
            mana_repo=None,
            loan_service=None,
        )

    return _make


def test_apply_loan_modifiers_white(effects_service_factory):
    svc = effects_service_factory("White", "Plains")
    out = svc.apply_loan_modifiers(1, 99, base_fee_rate=0.20, base_limit=100)
    assert out["fee_rate"] == pytest.approx(0.10)
    assert out["limit"] == 100
    assert out["color"] == "White"


def test_apply_loan_modifiers_black_doubles_limit(effects_service_factory):
    svc = effects_service_factory("Black", "Swamp")
    out = svc.apply_loan_modifiers(1, 99, base_fee_rate=0.20, base_limit=100)
    assert out["fee_rate"] == pytest.approx(0.30)
    assert out["limit"] == 200
    assert out["color"] == "Black"


def test_apply_loan_modifiers_no_color_passthrough(effects_service_factory):
    svc = effects_service_factory(None)
    out = svc.apply_loan_modifiers(1, 99, base_fee_rate=0.20, base_limit=100)
    assert out["fee_rate"] == 0.20
    assert out["limit"] == 100
    assert out["color"] is None


def test_apply_loan_modifiers_when_tapped_passes_through(effects_service_factory):
    """Tapped mana suppresses all effects — loans use the base rate."""
    svc = effects_service_factory("White", "Plains", consumed=True)
    out = svc.apply_loan_modifiers(1, 99, base_fee_rate=0.20, base_limit=100)
    assert out["color"] is None
    assert out["fee_rate"] == 0.20
    assert out["limit"] == 100


def test_apply_sabotage_modifiers_red_halves_cost(effects_service_factory):
    svc = effects_service_factory("Red", "Mountain")
    out = svc.apply_sabotage_modifiers(1, 99, base_cost=10)
    assert out["cost"] == 5
    assert out["steal_depth_pct"] == 0.0


def test_apply_sabotage_modifiers_black_skim(effects_service_factory):
    svc = effects_service_factory("Black", "Swamp")
    out = svc.apply_sabotage_modifiers(1, 99, base_cost=10)
    assert out["cost"] == 10
    assert out["steal_depth_pct"] == 0.25


def test_get_boss_combat_modifiers_white(effects_service_factory):
    svc = effects_service_factory("White", "Plains")
    out = svc.get_boss_combat_modifiers(1, 99)
    assert out["damage_mult"] == 1.20
    assert out["hp_mult"] == 1.0
    assert out["loot_mult"] == 1.0


def test_get_boss_combat_modifiers_black_tankier_droppier(effects_service_factory):
    svc = effects_service_factory("Black", "Swamp")
    out = svc.get_boss_combat_modifiers(1, 99)
    assert out["damage_mult"] == 1.0
    assert out["hp_mult"] == 1.30
    assert out["loot_mult"] == 1.25


def test_get_boss_combat_modifiers_blue_reveals_hp(effects_service_factory):
    svc = effects_service_factory("Blue", "Island")
    out = svc.get_boss_combat_modifiers(1, 99)
    assert out["reveal_hp"] is True


def test_get_weather_combo_modifiers_storm_blue_cooldown(effects_service_factory):
    svc = effects_service_factory("Blue", "Island")
    out = svc.get_weather_combo_modifiers(1, 99, "storm")
    assert out["applied"] is True
    assert out["cooldown_mult"] == 0.5


def test_get_weather_combo_modifiers_sunny_white_yield(effects_service_factory):
    svc = effects_service_factory("White", "Plains")
    out = svc.get_weather_combo_modifiers(1, 99, "sunny")
    assert out["applied"] is True
    assert out["yield_mult"] == 1.10


def test_get_weather_combo_modifiers_mismatch_no_op(effects_service_factory):
    """Storm + White: not Blue, so no combo applies."""
    svc = effects_service_factory("White", "Plains")
    out = svc.get_weather_combo_modifiers(1, 99, "storm")
    assert out["applied"] is False
    assert out["cooldown_mult"] == 1.0
    assert out["yield_mult"] == 1.0


def test_get_weather_combo_modifiers_no_weather(effects_service_factory):
    svc = effects_service_factory("Blue", "Island")
    out = svc.get_weather_combo_modifiers(1, 99, None)
    assert out["applied"] is False
