"""Targeted tests for the boss revamp: scaling, lum penalty, win-cap,
persisted HP, dialogue v2 slot pick.

These exercise the helpers added in services/dig_service.py and the
new constants in services/dig_constants.py without requiring a full
encounter loop.
"""

from __future__ import annotations

import pytest

from services.dig_constants import (
    BOSS_ARCHETYPES,
    BOSS_DIALOGUE_V2,
    BOSS_HP_REGEN_PER_HOUR,
    BOSS_PRESTIGE_BONUS,
    BOSS_TIER_BONUS,
    LUMINOSITY_BRIGHT,
    LUMINOSITY_DARK,
    PHASE_TRANSITION_EVENTS,
    PINNACLE_BOSSES,
    PINNACLE_POOL_IDS,
    PINNACLE_RELIC_BASE_NAME,
    PINNACLE_RELIC_STAT_POOL,
    PINNACLE_RELIC_SUFFIX_POOL,
    WIN_CHANCE_CAP,
)
from services.dig_service import (
    DigService,
    _approx_duel_win_prob,
    _luminosity_combat_penalty,
)

# --- Luminosity combat penalty ----------------------------------------


class TestLuminosityCombatPenalty:
    def test_bright_no_penalty(self):
        assert _luminosity_combat_penalty(100) == (0.0, 0)
        assert _luminosity_combat_penalty(LUMINOSITY_BRIGHT) == (0.0, 0)

    def test_dim_minus_3pct_hit(self):
        offset, dmg = _luminosity_combat_penalty(50)
        assert offset == pytest.approx(-0.03)
        assert dmg == 0

    def test_dark_minus_8pct_hit(self):
        offset, dmg = _luminosity_combat_penalty(LUMINOSITY_DARK)
        assert offset == pytest.approx(-0.08)
        assert dmg == 0

    def test_pitch_minus_15pct_hit_plus_1_dmg(self):
        offset, dmg = _luminosity_combat_penalty(0)
        assert offset == pytest.approx(-0.15)
        assert dmg == 1

    def test_below_dim_threshold_uses_dark(self):
        # Lum 25 = inside Dark band (1..25)
        offset, dmg = _luminosity_combat_penalty(25)
        assert offset == pytest.approx(-0.08)


# --- Win-chance cap ---------------------------------------------------


class TestWinChanceCap:
    def test_cap_applies_when_overwhelming_advantage(self):
        # Player overwhelms boss: high HP, high hit, boss is glass.
        # Without cap this would round to 1.00; cap clamps to 0.95.
        win = _approx_duel_win_prob(
            player_hp=20, boss_hp=2,
            player_hit=0.95, player_dmg=5,
            boss_hit=0.05, boss_dmg=1,
            trials=200,
        )
        assert win <= WIN_CHANCE_CAP + 1e-9

    def test_low_win_unchanged(self):
        # When the genuine win prob is already low, cap is a no-op.
        win = _approx_duel_win_prob(
            player_hp=2, boss_hp=20,
            player_hit=0.10, player_dmg=1,
            boss_hit=0.95, boss_dmg=5,
            trials=200,
        )
        assert win < 0.5


# --- Boss-stat scaling helper ----------------------------------------


class TestScaleBossStats:
    def setup_method(self):
        # Lightweight DigService stub: only needs the helper, not repos.
        self.service = DigService.__new__(DigService)

    def test_archetype_hp_multiplier_applied(self):
        base = {"player_hp": 5, "boss_hp": 4, "player_hit": 0.6,
                "player_dmg": 1, "boss_hit": 0.3, "boss_dmg": 1}
        # Tank archetype: hp_mult 1.5 → base 4 * 1.5 = 6.
        # at_boss=24 falls back to BOSS_TIER_BONUS[25] (defaultdict-style),
        # whose hp bonus is 0, so the result stays at 6.
        scaled = self.service._scale_boss_stats(
            base, boss_id="pudge", at_boss=24, prestige_level=0,
        )
        assert scaled["boss_hp"] == 6

    def test_glass_cannon_dmg_offset(self):
        base = {"player_hp": 5, "boss_hp": 4, "player_hit": 0.6,
                "player_dmg": 1, "boss_hit": 0.3, "boss_dmg": 1}
        scaled = self.service._scale_boss_stats(
            base, boss_id="lina", at_boss=0, prestige_level=0,
        )
        # Glass Cannon dmg_offset=1; base+offset = 2
        assert scaled["boss_dmg"] == 2

    def test_depth_scaling_hp(self):
        base = {"player_hp": 5, "boss_hp": 4, "player_hit": 0.6,
                "player_dmg": 1, "boss_hit": 0.3, "boss_dmg": 1}
        # Bruiser archetype at the pinnacle (depth 300) gets BOSS_TIER_BONUS[300]['hp'].
        scaled = self.service._scale_boss_stats(
            base, boss_id="grothak", at_boss=300, prestige_level=0,
        )
        assert scaled["boss_hp"] == 4 + int(BOSS_TIER_BONUS[300]["hp"])

    def test_depth_scaling_hit_and_dmg(self):
        base = {"player_hp": 5, "boss_hp": 4, "player_hit": 0.6,
                "player_dmg": 1, "boss_hit": 0.3, "boss_dmg": 1}
        # Bruiser at depth 300 (pinnacle): boss_hit/dmg = base + tier table entry.
        scaled = self.service._scale_boss_stats(
            base, boss_id="grothak", at_boss=300, prestige_level=0,
        )
        assert scaled["boss_hit"] == pytest.approx(0.30 + BOSS_TIER_BONUS[300]["hit"])
        assert scaled["boss_dmg"] == 1 + int(BOSS_TIER_BONUS[300]["dmg"])

    def test_prestige_scaling(self):
        base = {"player_hp": 5, "boss_hp": 4, "player_hit": 0.6,
                "player_dmg": 1, "boss_hit": 0.3, "boss_dmg": 1}
        # Bruiser, P5 (Void unlock cushion): boss stats per BOSS_PRESTIGE_BONUS[5].
        # at_boss=25 falls back to BOSS_TIER_BONUS[25] which is all zeros.
        scaled = self.service._scale_boss_stats(
            base, boss_id="grothak", at_boss=25, prestige_level=5,
        )
        pb = BOSS_PRESTIGE_BONUS[5]
        assert scaled["boss_hp"] == 4 + int(pb["hp"])
        assert scaled["boss_hit"] == pytest.approx(0.30 + pb["hit"])
        assert scaled["boss_dmg"] == 1 + int(pb["dmg"])

    def test_non_boundary_depth_falls_back_to_lower_tier(self):
        """A depth between two boundaries should pick up the lower tier's bonus.

        ``_scale_boss_stats`` is called from production code only at boss
        boundaries, but defensive callers may pass intermediate depths.
        The fallback should land on ``max(k for k in BOSS_TIER_BONUS if k <= at_boss)``.
        """
        base = {"player_hp": 5, "boss_hp": 4, "player_hit": 0.6,
                "player_dmg": 1, "boss_hit": 0.3, "boss_dmg": 1}
        # depth 130 is between T100 and T150. Should use T100's row.
        scaled = self.service._scale_boss_stats(
            base, boss_id="grothak", at_boss=130, prestige_level=0,
        )
        assert scaled["boss_hp"] == 4 + int(BOSS_TIER_BONUS[100]["hp"])
        # depth 250 is between T200 and T275. Should use T200's row.
        scaled = self.service._scale_boss_stats(
            base, boss_id="grothak", at_boss=250, prestige_level=0,
        )
        assert scaled["boss_hp"] == 4 + int(BOSS_TIER_BONUS[200]["hp"])

    def test_echo_applies_25pct_hp_discount(self):
        base = {"player_hp": 5, "boss_hp": 4, "player_hit": 0.6,
                "player_dmg": 1, "boss_hit": 0.3, "boss_dmg": 1}
        no_echo = self.service._scale_boss_stats(
            base, boss_id="grothak", at_boss=300, prestige_level=0, echo_applied=False,
        )
        with_echo = self.service._scale_boss_stats(
            base, boss_id="grothak", at_boss=300, prestige_level=0, echo_applied=True,
        )
        assert with_echo["boss_hp"] == max(1, int(round(no_echo["boss_hp"] * 0.75)))


# --- Persisted boss HP -----------------------------------------------


class TestPersistedBossHP:
    def setup_method(self):
        self.service = DigService.__new__(DigService)

    def test_no_persisted_hp_returns_fresh(self):
        bp = {}
        hp, hp_max = self.service._resolve_persisted_boss_hp(bp, 25, fresh_hp=10, now=0)
        assert hp == 10
        assert hp_max == 10

    def test_legacy_string_treated_as_fresh(self):
        bp = {"25": "active"}
        hp, hp_max = self.service._resolve_persisted_boss_hp(bp, 25, fresh_hp=10, now=0)
        assert hp == 10
        assert hp_max == 10

    def test_persisted_hp_used_when_present(self):
        bp = {"25": {"hp_remaining": 4, "hp_max": 12, "last_engaged_at": 0}}
        hp, hp_max = self.service._resolve_persisted_boss_hp(bp, 25, fresh_hp=12, now=0)
        # 0 hours elapsed, no regen; persisted HP applies.
        assert hp == 4
        assert hp_max == 12

    def test_regen_caps_at_hp_max(self):
        # 100 hours elapsed at +1/hour would exceed hp_max; should cap.
        now = 100 * 3600
        bp = {"25": {"hp_remaining": 4, "hp_max": 12, "last_engaged_at": 0}}
        hp, hp_max = self.service._resolve_persisted_boss_hp(bp, 25, fresh_hp=12, now=now)
        assert hp == 12  # capped at hp_max
        assert hp_max == 12

    def test_regen_partial(self):
        # 3 hours → +3 HP; persisted 4 → 7 (within cap).
        now = 3 * 3600
        bp = {"25": {"hp_remaining": 4, "hp_max": 12, "last_engaged_at": 0}}
        hp, _ = self.service._resolve_persisted_boss_hp(bp, 25, fresh_hp=12, now=now)
        assert hp == 4 + 3 * BOSS_HP_REGEN_PER_HOUR

    def test_persist_after_loss_writes_entry(self):
        bp = {}
        self.service._persist_boss_hp_after_fight(
            bp, 25, "grothak",
            ending_hp=3, hp_max=10,
            won=False, outcome="loss", now=42,
        )
        entry = bp["25"]
        assert entry["hp_remaining"] == 3
        assert entry["hp_max"] == 10
        assert entry["last_engaged_at"] == 42
        assert entry["last_outcome"] == "loss"
        assert entry["first_meet_seen"] is True
        assert entry["status"] == "active"

    def test_persist_preserves_existing_status(self):
        bp = {"25": {"status": "phase1_defeated", "boss_id": "pudge"}}
        self.service._persist_boss_hp_after_fight(
            bp, 25, "pudge",
            ending_hp=2, hp_max=8,
            won=False, outcome="loss", now=100,
        )
        # status carries over (phase1_defeated, not active)
        assert bp["25"]["status"] == "phase1_defeated"


# --- Dialogue v2 slot picking ----------------------------------------


class TestDialogueV2:
    def test_all_21_bosses_have_first_meet(self):
        expected_bosses = [
            "grothak", "pudge", "ogre_magi",
            "crystalia", "crystal_maiden", "tusk",
            "magmus_rex", "lina", "doom",
            "void_warden", "spectre", "void_spirit",
            "sporeling_sovereign", "treant_protector", "broodmother",
            "chronofrost", "faceless_void", "weaver",
            "nameless_depth", "oracle", "terrorblade",
        ]
        for boss_id in expected_bosses:
            assert boss_id in BOSS_DIALOGUE_V2, f"missing {boss_id}"
            slots = BOSS_DIALOGUE_V2[boss_id]
            assert "first_meet" in slots
            assert len(slots["first_meet"]) >= 2

    def test_pinnacle_pool_has_all_three(self):
        for pid in PINNACLE_POOL_IDS:
            assert pid in PINNACLE_BOSSES
            assert pid in BOSS_DIALOGUE_V2
            assert pid in PINNACLE_RELIC_BASE_NAME

    def test_render_boss_bark_substitutes_tokens(self):
        service = DigService.__new__(DigService)
        tunnel = {
            "streak_days": 7, "depth": 50, "prestige_level": 2,
            "boss_progress": '{"25": {"status": "defeated", "boss_id": "grothak"}}',
        }
        rendered = service._render_boss_bark(
            "Streak {streak} days at depth {depth}, P{prestige}.", tunnel,
        )
        assert "7 days" in rendered
        assert "depth 50" in rendered
        assert "P2" in rendered

    def test_render_boss_bark_no_tokens_passes_through(self):
        service = DigService.__new__(DigService)
        rendered = service._render_boss_bark("No substitution here.", {})
        assert rendered == "No substitution here."

    def test_render_boss_bark_killed_boss_fallback(self):
        service = DigService.__new__(DigService)
        # No defeated bosses → falls back to "the early dark"
        rendered = service._render_boss_bark(
            "You who slew {killed_boss_name}.",
            {"streak_days": 1, "depth": 0, "prestige_level": 0,
             "boss_progress": "{}"},
        )
        assert "the early dark" in rendered


# --- Pinnacle wiring (data only) -------------------------------------


class TestPinnacleConstants:
    def test_pinnacle_pool_size(self):
        assert len(PINNACLE_POOL_IDS) == 3

    def test_each_pinnacle_has_three_phases(self):
        for pid, pinnacle in PINNACLE_BOSSES.items():
            assert len(pinnacle.phases) == 3

    def test_relic_pool_non_empty(self):
        assert len(PINNACLE_RELIC_STAT_POOL) >= 8
        assert len(PINNACLE_RELIC_SUFFIX_POOL) >= 6

    def test_phase_event_pool_non_empty(self):
        assert len(PHASE_TRANSITION_EVENTS) >= 4


# --- Archetype map ---------------------------------------------------


class TestArchetypeMap:
    def test_known_archetypes_defined(self):
        for name in ("tank", "bruiser", "glass_cannon", "slippery"):
            assert name in BOSS_ARCHETYPES
            entry = BOSS_ARCHETYPES[name]
            assert "hp_mult" in entry
            assert "hit_offset" in entry
            assert "dmg_offset" in entry
