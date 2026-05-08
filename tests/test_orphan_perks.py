"""Tests for the implemented orphan prestige perks: veteran_miner,
tunnel_mastery, reading_the_stone."""
from __future__ import annotations

import pytest

from services.dig_service import DigService


class TestEventHasRisk:
    """The veteran_miner perk only fires on events where a negative outcome is
    possible. _event_has_risk encodes that 'could have lost JC' check."""

    def test_event_with_negative_failure_is_risky(self):
        event = {
            "risky_option": {
                "success": {"jc": 5},
                "failure": {"jc": -3},
            }
        }
        assert DigService._event_has_risk(event) is True

    def test_event_with_only_positive_outcomes_is_not_risky(self):
        event = {
            "safe_option": {
                "success": {"jc": 2},
                "failure": {"jc": 0},
            }
        }
        assert DigService._event_has_risk(event) is False

    def test_event_with_negative_jc_range_is_risky(self):
        event = {
            "risky_option": {
                "success": {"jc": [3, 8]},
                "failure": {"jc": [-5, -1]},
            }
        }
        assert DigService._event_has_risk(event) is True

    def test_legacy_outcomes_format_with_negative(self):
        event = {
            "outcomes": {
                "safe": {"jc": 1},
                "risky": {"jc": -10},
            }
        }
        assert DigService._event_has_risk(event) is True

    def test_empty_event_is_not_risky(self):
        assert DigService._event_has_risk({}) is False


class TestReadingTheStoneHint:
    """The reading_the_stone perk picks the option with highest EV and returns
    a flavor whisper — never numbers."""

    def test_returns_none_for_event_with_no_options(self):
        from commands.dig import _reading_the_stone_hint
        assert _reading_the_stone_hint({}) is None
        assert _reading_the_stone_hint({"name": "no choices"}) is None

    def test_picks_safe_when_safe_has_higher_ev(self):
        from commands.dig import _READING_HINTS, _reading_the_stone_hint
        event = {
            "safe_option": {
                "success_chance": 1.0,
                "success": {"jc": 10},
                "failure": {"jc": 0},
            },
            "risky_option": {
                "success_chance": 0.5,
                "success": {"jc": 5},
                "failure": {"jc": -10},
            },
        }
        hint = _reading_the_stone_hint(event)
        assert hint in _READING_HINTS["safe"]

    def test_picks_risky_when_risky_has_higher_ev(self):
        from commands.dig import _READING_HINTS, _reading_the_stone_hint
        event = {
            "safe_option": {
                "success_chance": 1.0,
                "success": {"jc": 1},
                "failure": {"jc": 0},
            },
            "risky_option": {
                "success_chance": 0.9,
                "success": {"jc": 20},
                "failure": {"jc": 0},
            },
        }
        hint = _reading_the_stone_hint(event)
        assert hint in _READING_HINTS["risky"]

    def test_hint_contains_no_numbers(self):
        # Atmospheric only — never expose EV math. Loop a few different
        # events to catch any accidental integer that snuck in.
        from commands.dig import _reading_the_stone_hint
        events = [
            {
                "safe_option": {"success_chance": 1.0, "success": {"jc": 5}, "failure": {"jc": 0}},
                "risky_option": {"success_chance": 0.5, "success": {"jc": 10}, "failure": {"jc": -5}},
            },
            {
                "desperate_option": {"success_chance": 0.3, "success": {"jc": 50}, "failure": {"jc": -20}},
            },
        ]
        for event in events:
            hint = _reading_the_stone_hint(event)
            assert hint is not None
            assert not any(ch.isdigit() for ch in hint), f"hint leaks numbers: {hint}"


class TestTunnelMasteryAndVeteranMinerInjection:
    """Wire-up sanity: the perk-effect aggregator must surface the expected
    keys so resolve_event can read them. This guards against accidental
    rename-without-handler regressions."""

    def test_aggregator_exposes_orphan_keys(self):
        from services.dig_constants import PRESTIGE_PERK_VALUES

        # Each orphan must define exactly the key resolve_event reads.
        assert "risky_success_bonus" in PRESTIGE_PERK_VALUES["veteran_miner"]
        assert "expedition_reward_bonus" in PRESTIGE_PERK_VALUES["tunnel_mastery"]
        assert "event_choice_reveal" in PRESTIGE_PERK_VALUES["reading_the_stone"]

    def test_aggregator_sums_stacks(self):
        # Method exists on the class as static-style aggregator
        # so we can call it without a fully-wired DigService instance.
        from services.dig_service import DigService
        # Build a minimal DigService for invoking the bound method.
        # Using None for repos works because _aggregate_perk_effects only
        # reads PRESTIGE_PERK_VALUES, not any repo state.
        svc = DigService.__new__(DigService)
        agg = svc._aggregate_perk_effects(["loot_multiplier"] * 3)
        assert agg.get("jc_bonus") == 3.0

        agg2 = svc._aggregate_perk_effects(["veteran_miner"] * 2)
        assert agg2.get("risky_success_bonus") == pytest.approx(0.10)
