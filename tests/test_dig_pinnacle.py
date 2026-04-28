"""End-to-end tests for the pinnacle boss (depth 300).

Covers:
- pinnacle boundary detection requires all 7 tier bosses defeated
- ``_ensure_pinnacle_locked`` rolls one of three candidates and persists it
- 3-phase fight flow with persisted HP between encounters
- Phase 3 victory drops a relic with 2 random stat rolls and gates prestige
- Foreshadowing line surfaces after T275 cleared but pinnacle pending
"""

from __future__ import annotations

import json
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    BOSS_BOUNDARIES,
    PINNACLE_BOSSES,
    PINNACLE_DEPTH,
    PINNACLE_FORESHADOW_LINES,
    PINNACLE_POOL_IDS,
    PINNACLE_RELIC_BASE_NAME,
    PINNACLE_RELIC_SUFFIX_POOL,
)
from services.dig_service import DigService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repo, balance=2000):
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
