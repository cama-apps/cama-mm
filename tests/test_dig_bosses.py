"""Consolidated tests for the dig boss system.

Merges three previously fragmented files:
- Boss revamp (scaling, lum penalty, win-cap, persisted HP, dialogue v2)
- Boss echo (post-kill 24h softening window)
- Pinnacle boss (depth 300 fight flow, prestige gating, foreshadowing, relics)
"""

from __future__ import annotations

import json
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    BOSS_ARCHETYPES,
    BOSS_BOUNDARIES,
    BOSS_DIALOGUE_V2,
    BOSS_HP_REGEN_PER_2_HOURS,
    BOSS_PRESTIGE_BONUS,
    BOSS_TIER_BONUS,
    LUMINOSITY_BRIGHT,
    LUMINOSITY_DARK,
    PHASE_TRANSITION_EVENTS,
    PINNACLE_BOSSES,
    PINNACLE_DEPTH,
    PINNACLE_FORESHADOW_LINES,
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
from tests.conftest import TEST_GUILD_ID

# =============================================================================
# Shared fixtures and helpers
# =============================================================================


@pytest.fixture
def dig_repo(repo_db_path):
    """A DigRepository bound to the per-test schema template."""
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    """A DigService with weather effects stubbed out."""
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repo, balance=2000):
    """Register a deterministic test player at TEST_GUILD_ID with given balance."""
    player_repo.add(
        discord_id=10001,
        discord_username="User10001",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repo.update_balance(10001, TEST_GUILD_ID, balance)


def _all_tiers_cleared_progress() -> dict:
    """Return a boss_progress dict with all 7 tier bosses defeated."""
    return {str(b): "defeated" for b in BOSS_BOUNDARIES}


def _at_pinnacle(dig_repo, depth=299):
    """Place the player at the pinnacle threshold with all tiers cleared."""
    bp = _all_tiers_cleared_progress()
    dig_repo.update_tunnel(
        10001, TEST_GUILD_ID,
        depth=depth,
        boss_progress=json.dumps(bp),
        prestige_level=0,
    )


# =============================================================================
# === Revamp mechanics ===
# =============================================================================

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
        # 100 hours elapsed at +1/2h would exceed hp_max; should cap.
        now = 100 * 3600
        bp = {"25": {"hp_remaining": 4, "hp_max": 12, "last_engaged_at": 0}}
        hp, hp_max = self.service._resolve_persisted_boss_hp(bp, 25, fresh_hp=12, now=now)
        assert hp == 12  # capped at hp_max
        assert hp_max == 12

    def test_regen_partial(self):
        # 6 hours → 3 two-hour blocks → +3 HP; persisted 4 → 7 (within cap).
        now = 6 * 3600
        bp = {"25": {"hp_remaining": 4, "hp_max": 12, "last_engaged_at": 0}}
        hp, _ = self.service._resolve_persisted_boss_hp(bp, 25, fresh_hp=12, now=now)
        assert hp == 4 + 3 * BOSS_HP_REGEN_PER_2_HOURS

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


# =============================================================================
# === Echo variant ===
# =============================================================================


class TestBossEchoRepository:
    """Record, read, expire, and overwrite echo rows (keyed by boss_id)."""

    def test_no_row_returns_none(self, dig_repo):
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak") is None

    def test_record_then_read(self, dig_repo):
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "grothak", 25,
            killer_discord_id=777, window_seconds=3600,
        )
        row = dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak")
        assert row is not None
        assert row["killer_discord_id"] == 777
        assert row["depth"] == 25
        assert row["weakened_until"] > int(time.time())

    def test_expired_row_returns_none(self, dig_repo, monkeypatch):
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "crystalia", 50,
            killer_discord_id=111, window_seconds=60,
        )
        real_time = time.time()
        monkeypatch.setattr(time, "time", lambda: real_time + 3600)
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "crystalia") is None

    def test_overwrite_restarts_window(self, dig_repo):
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "magmus_rex", 75,
            killer_discord_id=111, window_seconds=60,
        )
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "magmus_rex", 75,
            killer_discord_id=222, window_seconds=3600,
        )
        row = dig_repo.get_active_boss_echo(TEST_GUILD_ID, "magmus_rex")
        assert row["killer_discord_id"] == 222

    def test_boss_id_isolation(self, dig_repo):
        """Killing Pudge at tier 25 does NOT weaken Grothak at tier 25."""
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "pudge", 25,
            killer_discord_id=1, window_seconds=3600,
        )
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "crystalia", 50,
            killer_discord_id=2, window_seconds=3600,
        )
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "pudge")["killer_discord_id"] == 1
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "crystalia")["killer_discord_id"] == 2
        # Same tier, different boss — no echo crosstalk.
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak") is None
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "ogre_magi") is None

    def test_guild_isolation(self, dig_repo):
        dig_repo.record_boss_echo(
            1000, "grothak", 25,
            killer_discord_id=1, window_seconds=3600,
        )
        dig_repo.record_boss_echo(
            2000, "grothak", 25,
            killer_discord_id=2, window_seconds=3600,
        )
        assert dig_repo.get_active_boss_echo(1000, "grothak")["killer_discord_id"] == 1
        assert dig_repo.get_active_boss_echo(2000, "grothak")["killer_discord_id"] == 2
        # None guild normalizes to 0; not a collision with either above.
        assert dig_repo.get_active_boss_echo(None, "grothak") is None


# =============================================================================
# === Pinnacle ===
# =============================================================================

# --- Boundary detection ----------------------------------------------


class TestPinnacleBoundary:
    def test_pinnacle_boundary_requires_all_tiers_defeated(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """At depth 299 with one tier still active, pinnacle does NOT trigger."""
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)

        partial = _all_tiers_cleared_progress()
        partial["275"] = "active"  # leave the last tier undefeated
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=299,
            boss_progress=json.dumps(partial),
        )
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        bp = dig_service._get_boss_progress(dict(tunnel))
        assert dig_service._at_boss_boundary(299, bp) is None

    def test_pinnacle_boundary_fires_when_tiers_cleared(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)

        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        bp = dig_service._get_boss_progress(dict(tunnel))
        assert dig_service._at_boss_boundary(299, bp) == PINNACLE_DEPTH

    def test_pinnacle_does_not_re_fire_after_defeat(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)

        bp = _all_tiers_cleared_progress()
        bp["300"] = "defeated"
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=299,
            boss_progress=json.dumps(bp),
        )
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        bp_dict = dig_service._get_boss_progress(dict(tunnel))
        assert dig_service._at_boss_boundary(299, bp_dict) is None


class TestPinnacleReproc:
    """Catch-up: a player who tunneled past the pinnacle without
    defeating it (legacy tunnels, or skipped encounters) gets the
    pinnacle re-prompted at depth >= PINNACLE_REPROC_DEPTH so prestige
    isn't permanently locked out."""

    def test_reproc_fires_past_threshold_when_pinnacle_undefeated(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        from services.dig_constants import PINNACLE_REPROC_DEPTH

        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)

        bp = _all_tiers_cleared_progress()  # tier bosses defeated, pinnacle absent
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=PINNACLE_REPROC_DEPTH + 56,  # 456: Steve W.'s actual depth
            boss_progress=json.dumps(bp),
        )
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        bp_dict = dig_service._get_boss_progress(dict(tunnel))
        assert dig_service._at_boss_boundary(
            PINNACLE_REPROC_DEPTH + 56, bp_dict
        ) == PINNACLE_DEPTH

    def test_reproc_fires_at_hard_cap_so_prestige_can_unlock(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A player at depth 500 (hard cap) with an undefeated pinnacle
        should still see the pinnacle proc — otherwise they're stuck
        unable to dig (cap) and unable to prestige (pinnacle gate)."""
        from services.dig_constants import PRESTIGE_HARD_CAP

        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)

        bp = _all_tiers_cleared_progress()
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=PRESTIGE_HARD_CAP,
            boss_progress=json.dumps(bp),
        )
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        bp_dict = dig_service._get_boss_progress(dict(tunnel))
        assert dig_service._at_boss_boundary(
            PRESTIGE_HARD_CAP, bp_dict
        ) == PINNACLE_DEPTH

    def test_reproc_does_not_fire_if_tier_bosses_remain(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Catch-up shouldn't trip if tier bosses are still active —
        only legacy tunnels that cleared everything but the pinnacle."""
        from services.dig_constants import PINNACLE_REPROC_DEPTH

        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)

        bp = _all_tiers_cleared_progress()
        bp["275"] = "active"  # one tier boss still pending
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=PINNACLE_REPROC_DEPTH + 50,
            boss_progress=json.dumps(bp),
        )
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        bp_dict = dig_service._get_boss_progress(dict(tunnel))
        assert dig_service._at_boss_boundary(
            PINNACLE_REPROC_DEPTH + 50, bp_dict
        ) is None

    def test_reproc_does_not_fire_if_pinnacle_already_defeated(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A defeated pinnacle stays defeated — no re-fight loop."""
        from services.dig_constants import PINNACLE_REPROC_DEPTH

        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)

        bp = _all_tiers_cleared_progress()
        bp[str(PINNACLE_DEPTH)] = "defeated"
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=PINNACLE_REPROC_DEPTH + 50,
            boss_progress=json.dumps(bp),
        )
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        bp_dict = dig_service._get_boss_progress(dict(tunnel))
        assert dig_service._at_boss_boundary(
            PINNACLE_REPROC_DEPTH + 50, bp_dict
        ) is None

    def test_reproc_does_not_fire_below_threshold(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Below the catch-up threshold, the boundary is the only
        trigger — depth 350 (after the pinnacle but before reproc)
        should NOT spawn the boss."""
        from services.dig_constants import PINNACLE_REPROC_DEPTH

        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)

        bp = _all_tiers_cleared_progress()  # pinnacle absent
        below = PINNACLE_REPROC_DEPTH - 50
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=below,
            boss_progress=json.dumps(bp),
        )
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        bp_dict = dig_service._get_boss_progress(dict(tunnel))
        assert dig_service._at_boss_boundary(below, bp_dict) is None


# --- Pinnacle locking ------------------------------------------------


class TestPinnacleLock:
    def test_lock_picks_from_pool_and_persists(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)

        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        pid = dig_service._ensure_pinnacle_locked(10001, TEST_GUILD_ID, tunnel)
        assert pid in PINNACLE_POOL_IDS

        # Idempotent: second call returns same id.
        tunnel2 = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        pid2 = dig_service._ensure_pinnacle_locked(10001, TEST_GUILD_ID, tunnel2)
        assert pid == pid2


# --- Fight flow ------------------------------------------------------


class TestPinnacleFight:
    def test_phase1_win_advances_to_phase2(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, pinnacle_boss_id="first_digger")

        # Phase 1 of first_digger has a mid-fight mechanic. Disable it for
        # this test (we're verifying phase resolution, not the mechanic
        # prompt itself; that's covered separately).
        import domain.models.boss_mechanics as _bm
        monkeypatch.setattr(_bm, "get_mechanic", lambda mid: None)

        # Pinnacle phase 1 is intentionally tough — player_dmg=1 vs ~13 HP.
        # Force the duel deterministic by alternating: player hit roll = 0.0
        # (always hits), boss hit roll = 0.99 (always misses).
        roll_seq = iter([0.0, 0.99] * 50)
        monkeypatch.setattr(random, "random", lambda: next(roll_seq))
        monkeypatch.setattr(random, "randint", lambda lo, hi: lo)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=0)
        assert result["success"]
        assert result["won"] is True
        assert result.get("is_pinnacle") is True
        assert result["phase"] == 1
        assert result.get("phase2_incoming") is True

        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert tunnel["pinnacle_phase"] == 2
        bp = json.loads(tunnel["boss_progress"])
        entry = bp["300"]
        status = entry.get("status") if isinstance(entry, dict) else entry
        assert status == "phase1_defeated"

    def test_pinnacle_mechanic_pauses_then_resume_resolves(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A pinnacle phase rolls a mechanic, fight pauses with a
        ``pending_prompt``, and resume_boss_duel applies the option."""
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, pinnacle_boss_id="forgotten_king")

        # All rounds should reach the mechanic before either side dies.
        # Phase 1 of forgotten_king has mechanic king_decree (trigger_round=3).
        # We just need the duel loop to survive to round 3.
        roll_seq = iter([0.0, 0.99] * 50)
        monkeypatch.setattr(random, "random", lambda: next(roll_seq))

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=0)
        assert result["success"]
        assert "pending_prompt" in result
        assert result["mechanic_id"] == "king_decree"
        assert result["is_pinnacle"] is True
        assert result["phase"] == 1

        # An active duel row should now exist.
        active = dig_repo.get_active_duel(10001, TEST_GUILD_ID)
        assert active is not None
        assert active["boss_id"] == "forgotten_king"

        # Resume with option 0 (kneel — high success rate).
        resumed = dig_service.resume_boss_duel(10001, TEST_GUILD_ID, option_idx=0)
        assert resumed["success"]
        # Outcome is either won (True/False) — no longer pending.
        assert "pending_prompt" not in resumed
        # Active duel row was cleared.
        assert dig_repo.get_active_duel(10001, TEST_GUILD_ID) is None

    def test_arithmetic_challenge_correct_in_time(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Correct + fast arithmetic submission resolves as option 0 (best)."""
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            pinnacle_boss_id="forgotten_king", pinnacle_phase=3,
        )
        # Pin which mechanic rolls — phase 3 pool now has 3 entries.
        from domain.models import boss_mechanics as _bm
        original_get = _bm.get_mechanic
        monkeypatch.setattr(
            random.Random, "choice",
            lambda self, seq: "pinnacle_arithmetic_challenge",
        )
        # Survive to round 3 so the mechanic fires.
        roll_seq = iter([0.0, 0.99] * 50)
        monkeypatch.setattr(random, "random", lambda: next(roll_seq))

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=0)
        assert result["success"]
        assert "pending_prompt" in result
        challenge = result.get("timed_challenge")
        assert challenge is not None
        assert challenge["kind"] == "arithmetic"

        # Read the persisted challenge to compute the correct answer.
        state = dig_repo.get_active_duel(10001, TEST_GUILD_ID)
        status = json.loads(state["status_effects"])
        expected = status["timed_challenge"]["answer_canonical"]

        # Submit within time window (clock didn't advance).
        resumed = dig_service.submit_timed_answer(10001, TEST_GUILD_ID, expected)
        assert resumed["success"]
        res = resumed["timed_challenge_resolution"]
        assert res["correct"] is True
        assert res["in_time"] is True
        assert res["option_idx"] == 0
        # Restore so other tests aren't affected.
        monkeypatch.setattr(_bm, "get_mechanic", original_get)

    def test_arithmetic_challenge_wrong_answer(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            pinnacle_boss_id="forgotten_king", pinnacle_phase=3,
        )
        monkeypatch.setattr(
            random.Random, "choice",
            lambda self, seq: "pinnacle_arithmetic_challenge",
        )
        roll_seq = iter([0.0, 0.99] * 50)
        monkeypatch.setattr(random, "random", lambda: next(roll_seq))

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=0)
        assert "pending_prompt" in result

        resumed = dig_service.submit_timed_answer(10001, TEST_GUILD_ID, "definitely-wrong")
        assert resumed["success"]
        assert resumed["timed_challenge_resolution"]["correct"] is False
        assert resumed["timed_challenge_resolution"]["option_idx"] == 2

    def test_riddle_challenge_correct_synonym(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Riddle accepts any synonym from its pool."""
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            pinnacle_boss_id="forgotten_king", pinnacle_phase=3,
        )
        # Force riddle mechanic.
        monkeypatch.setattr(
            random.Random, "choice",
            lambda self, seq: "pinnacle_riddle_challenge",
        )
        roll_seq = iter([0.0, 0.99] * 50)
        monkeypatch.setattr(random, "random", lambda: next(roll_seq))

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=0)
        assert "pending_prompt" in result
        state = dig_repo.get_active_duel(10001, TEST_GUILD_ID)
        status = json.loads(state["status_effects"])
        accepted = status["timed_challenge"]["accepted"]
        # Submit the LAST synonym (just to verify any synonym matches).
        resumed = dig_service.submit_timed_answer(10001, TEST_GUILD_ID, accepted[-1])
        assert resumed["timed_challenge_resolution"]["correct"] is True

    def test_loss_persists_phase_hp(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)

        # Pin a Tank phase-1 (forgotten_king) so reckless+miss-everything loses.
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, pinnacle_boss_id="forgotten_king")
        # Disable the mechanic prompt — testing the loss path's HP persistence.
        import domain.models.boss_mechanics as _bm
        monkeypatch.setattr(_bm, "get_mechanic", lambda mid: None)
        # Reckless (low hit, low HP) + every roll miss-rate → guaranteed loss.
        monkeypatch.setattr(random, "random", lambda: 0.999)
        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "reckless", wager=0)
        assert result["won"] is False
        assert result["boundary"] == PINNACLE_DEPTH
        # Phase HP persisted per-phase under "300:1"
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        bp = json.loads(tunnel["boss_progress"])
        assert "300:1" in bp
        assert bp["300:1"]["hp_remaining"] >= 0

    def test_double_submit_timed_answer_is_safe(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A second concurrent submit_timed_answer call must not double-resolve.

        Models a Discord modal on_submit retry: two callbacks fire for the
        same prompt. The first claims the row atomically and resolves; the
        second must see no active duel and return an error rather than
        applying the option_outcome a second time.
        """
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            pinnacle_boss_id="forgotten_king", pinnacle_phase=3,
        )
        monkeypatch.setattr(
            random.Random, "choice",
            lambda self, seq: "pinnacle_arithmetic_challenge",
        )
        roll_seq = iter([0.0, 0.99] * 50)
        monkeypatch.setattr(random, "random", lambda: next(roll_seq))
        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=0)
        assert "pending_prompt" in result

        first = dig_service.submit_timed_answer(10001, TEST_GUILD_ID, "definitely-wrong")
        assert first["success"]
        # Row is gone — concurrent retry must bail out, not re-resolve.
        second = dig_service.submit_timed_answer(10001, TEST_GUILD_ID, "definitely-wrong")
        assert second["success"] is False
        assert "no active" in (second.get("error") or "").lower()


# --- Prestige gating -------------------------------------------------


class TestPinnaclePrestigeGate:
    def test_cannot_prestige_without_pinnacle_defeat(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp = _all_tiers_cleared_progress()  # 7 tiers cleared, no pinnacle
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=300,
            boss_progress=json.dumps(bp),
        )
        check = dig_service.can_prestige(10001, TEST_GUILD_ID)
        assert check["can_prestige"] is False
        assert "stirs deeper" in (check.get("reason") or "")

    def test_can_prestige_with_pinnacle_defeated(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp = _all_tiers_cleared_progress()
        bp["300"] = "defeated"
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=300,
            boss_progress=json.dumps(bp),
        )
        check = dig_service.can_prestige(10001, TEST_GUILD_ID)
        assert check["can_prestige"] is True

    def test_prestige_resets_pinnacle_state(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp = _all_tiers_cleared_progress()
        bp["300"] = "defeated"
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=300,
            boss_progress=json.dumps(bp),
            pinnacle_boss_id="forgotten_king",
            pinnacle_phase=0,
        )
        result = dig_service.prestige(10001, TEST_GUILD_ID, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert tunnel.get("pinnacle_boss_id") in (None, "")
        assert (tunnel.get("pinnacle_phase") or 0) == 0

    def test_prestige_clears_stale_pinnacle_phase_keys(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Stale ``"300:N"`` composite phase keys in boss_progress must not
        survive a prestige reset, otherwise next-cycle pinnacle fights would
        pick up phantom HP from the previous run.
        """
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp = _all_tiers_cleared_progress()
        bp["300"] = "defeated"
        # Simulate stale per-phase HP entries left from this cycle's fight.
        bp["300:1"] = {"hp_remaining": 5, "hp_max": 18, "last_engaged_at": 999_000}
        bp["300:2"] = {"hp_remaining": 12, "hp_max": 18}
        bp["300:3"] = {"hp_remaining": 0, "hp_max": 21}
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=300, boss_progress=json.dumps(bp),
            pinnacle_boss_id="forgotten_king", pinnacle_phase=3,
        )
        result = dig_service.prestige(10001, TEST_GUILD_ID, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        new_bp = json.loads(tunnel["boss_progress"])
        for key in ("300", "300:1", "300:2", "300:3"):
            assert key not in new_bp, f"stale key {key!r} survived prestige reset"


# --- Foreshadowing ---------------------------------------------------


class TestPinnacleForeshadow:
    def test_foreshadow_after_t275_cleared(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp = _all_tiers_cleared_progress()
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID, depth=290,
            boss_progress=json.dumps(bp),
        )
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        line = dig_service._pinnacle_foreshadow_line(tunnel)
        assert line is not None
        assert line in PINNACLE_FORESHADOW_LINES

    def test_no_foreshadow_when_pinnacle_defeated(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp = _all_tiers_cleared_progress()
        bp["300"] = "defeated"
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID, depth=300,
            boss_progress=json.dumps(bp),
        )
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        assert dig_service._pinnacle_foreshadow_line(tunnel) is None

    def test_no_foreshadow_when_tiers_incomplete(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp = {str(b): "defeated" for b in BOSS_BOUNDARIES[:-1]}
        bp[str(BOSS_BOUNDARIES[-1])] = "active"
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID, depth=274,
            boss_progress=json.dumps(bp),
        )
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        assert dig_service._pinnacle_foreshadow_line(tunnel) is None


# --- Relic drop ------------------------------------------------------


class TestPinnacleRelicDrop:
    def test_drop_creates_artifact_with_two_stats(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        relic = dig_service._drop_pinnacle_relic(
            10001, TEST_GUILD_ID, tunnel, "forgotten_king",
        )
        assert relic["name"].startswith("Crown of ")
        assert len(relic["stats"]) == 2
        assert len(relic["stat_ids"]) == 2
        assert relic["stat_ids"][0] != relic["stat_ids"][1]
        # Artifact persisted in dig_artifacts.
        artifacts = dig_repo.get_artifacts(10001, TEST_GUILD_ID)
        assert any(a["artifact_id"].startswith("pinnacle:") for a in artifacts)

    def test_drop_name_uses_pool_suffix(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        for pid in PINNACLE_POOL_IDS:
            relic = dig_service._drop_pinnacle_relic(
                10001, TEST_GUILD_ID, tunnel, pid,
            )
            base = PINNACLE_RELIC_BASE_NAME[pid]
            suffix = relic["name"][len(base) + 4:]  # strip "<base> of "
            assert suffix in PINNACLE_RELIC_SUFFIX_POOL


# --- Build-info -------------------------------------------------------


class TestPinnacleBuildInfo:
    def test_pinnacle_info_returns_phase_title(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        _at_pinnacle(dig_repo)
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            pinnacle_boss_id="forgotten_king", pinnacle_phase=2,
        )
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        info = dig_service._build_boss_info(10001, TEST_GUILD_ID, tunnel, PINNACLE_DEPTH)
        assert info["is_pinnacle"] is True
        assert info["phase"] == 2
        assert info["phase_total"] == 3
        # Phase 2 title for forgotten_king is "The Crowned Hunger".
        assert info["name"] == PINNACLE_BOSSES["forgotten_king"].phases[1].title
