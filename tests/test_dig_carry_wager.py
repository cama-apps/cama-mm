"""Multi-phase boss fight: wager + risk_tier carry forward across phases.

Covers the headline behavior change in this patch — phase 1 victory no longer
forfeits the wager. The original stake rides through phase 2/3 and is settled
on full defeat. Loss or retreat between phases drops the carry markers.
"""

import json
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    PINNACLE_BASE_JC_REWARD,
    PINNACLE_BOSSES,
    PINNACLE_DEPTH,
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


def _register(player_repository, discord_id=10001, guild_id=12345, balance=2000):
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 3:
        player_repository.update_balance(discord_id, guild_id, balance)
    return discord_id


def _seed_phase1_cleared(dig_repo, discord_id, guild_id, *, boundary=25):
    """Plant a tunnel parked at ``boundary`` with phase 1 already cleared and
    a carried wager (200 JC, bold) on the boss_progress entry. Mimics the
    state right after the player won phase 1 of a multi-phase fight.
    """
    dig_repo.create_tunnel(discord_id, guild_id, "TestTunnel")
    boss_progress = {
        str(boundary): {
            "boss_id": "grothak",
            "status": "phase1_defeated",
            "carried_wager": 200,
            "carried_risk_tier": "bold",
        }
    }
    dig_repo.update_tunnel(
        discord_id, guild_id,
        depth=boundary,
        prestige_level=2,  # phase 2 unlocks at P2+
        boss_progress=json.dumps(boss_progress),
    )


class TestCarriedWagerReadback:
    """get_carried_wager surfaces the carried state for the UI."""

    def test_returns_carry_when_set(self, dig_service, dig_repo, player_repository, guild_id):
        _register(player_repository)
        _seed_phase1_cleared(dig_repo, 10001, guild_id)

        carried = dig_service.get_carried_wager(10001, guild_id)
        assert carried is not None
        assert carried["wager"] == 200
        assert carried["risk_tier"] == "bold"
        assert carried["boundary"] == 25

    def test_returns_none_without_carry(self, dig_service, dig_repo, player_repository, guild_id):
        _register(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(10001, guild_id, depth=25, prestige_level=2)

        assert dig_service.get_carried_wager(10001, guild_id) is None

    def test_returns_none_when_no_tunnel(self, dig_service, player_repository, guild_id):
        _register(player_repository)
        assert dig_service.get_carried_wager(10001, guild_id) is None


class TestPhaseTransitionPersistsCarry:
    """Phase 1 victory writes carried_wager + carried_risk_tier on the entry."""

    def test_set_carried_wager_helper_writes_both_fields(self, dig_service):
        bp = {"25": {"status": "active", "boss_id": "grothak"}}
        dig_service._set_carried_wager(bp, 25, 150, "reckless")

        entry = bp["25"]
        assert entry["carried_wager"] == 150
        assert entry["carried_risk_tier"] == "reckless"
        # Existing fields preserved.
        assert entry["status"] == "active"
        assert entry["boss_id"] == "grothak"

    def test_clear_carried_wager_drops_only_carry_fields(self, dig_service):
        bp = {
            "25": {
                "status": "phase1_defeated",
                "boss_id": "grothak",
                "carried_wager": 200,
                "carried_risk_tier": "bold",
                "hp_max": 40,
            }
        }
        dig_service._clear_carried_wager(bp, 25)

        entry = bp["25"]
        assert "carried_wager" not in entry
        assert "carried_risk_tier" not in entry
        # Other fields untouched.
        assert entry["status"] == "phase1_defeated"
        assert entry["hp_max"] == 40


class TestRetreatForfeitsHalfOfCarry:
    """Retreating between phases forfeits half the carried wager."""

    def test_retreat_with_carried_wager_debits_half(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        _register(player_repository)
        _seed_phase1_cleared(dig_repo, 10001, guild_id)
        balance_before = player_repository.get_balance(10001, guild_id)

        result = dig_service.retreat_boss(10001, guild_id)

        assert result["success"]
        assert result["carried_wager_forfeit"] == 100  # 200 // 2
        assert player_repository.get_balance(10001, guild_id) == balance_before - 100

    def test_retreat_clears_carry_markers(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        _register(player_repository)
        _seed_phase1_cleared(dig_repo, 10001, guild_id)

        dig_service.retreat_boss(10001, guild_id)

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        boss_progress = json.loads(tunnel["boss_progress"])
        entry = boss_progress["25"]
        assert "carried_wager" not in entry
        assert "carried_risk_tier" not in entry

    def test_retreat_without_carry_does_not_charge(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        _register(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(10001, guild_id, depth=25, prestige_level=0)
        balance_before = player_repository.get_balance(10001, guild_id)

        result = dig_service.retreat_boss(10001, guild_id)

        assert result["success"]
        assert result["carried_wager_forfeit"] == 0
        assert player_repository.get_balance(10001, guild_id) == balance_before


def _setup_pinnacle(dig_repo, player_repository, guild_id, *, discord_id=20001,
                    balance=2000, prestige=0):
    """Register a player and park a tunnel at the pinnacle depth (350)."""
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"Pinnacle{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(discord_id, guild_id, balance)
    dig_repo.create_tunnel(discord_id, guild_id, "PinnacleTunnel")
    dig_repo.update_tunnel(
        discord_id, guild_id,
        depth=PINNACLE_DEPTH, max_depth=PINNACLE_DEPTH, prestige_level=prestige,
    )
    tunnel = dict(dig_repo.get_tunnel(discord_id, guild_id))
    tunnel["discord_id"] = discord_id
    return tunnel


def _finalize_pinnacle(dig_service, guild_id, *, tunnel, discord_id=20001,
                       pinnacle_id="forgotten_king", phase_idx=3, won=True,
                       wager=0, risk_tier="bold", prestige_level=0,
                       win_chance=0.55, boss_progress=None):
    """Drive _finalize_pinnacle_outcome with sane defaults for a pinnacle fight.

    win_chance defaults below the wager-taper knee so the authored multiplier
    applies untapered unless a test overrides it.
    """
    pinnacle = PINNACLE_BOSSES[pinnacle_id]
    return dig_service._finalize_pinnacle_outcome(
        discord_id=discord_id, guild_id=guild_id, tunnel=tunnel,
        pinnacle_id=pinnacle_id, pinnacle=pinnacle,
        phase_def=pinnacle.phases[phase_idx - 1],
        phase_idx=phase_idx, phase_key=f"{PINNACLE_DEPTH}:{phase_idx}",
        boss_progress=boss_progress if boss_progress is not None else {},
        won=won, boss_hp=0, boss_hp_max=500,
        risk_tier=risk_tier, wager=wager,
        win_chance=win_chance, attempts=1, round_log=[], gear_broken_names=[],
        prestige_level=prestige_level, depth=PINNACLE_DEPTH, now=int(time.time()),
    )


class TestPinnacleWagerPayout:
    """A pinnacle wager rides all 3 phases and pays out on a full clear at
    win-chance-tapered odds; any phase loss forfeits it. Previously the wager
    was ignored on a phase-3 win and could only ever be lost.
    """

    def test_full_clear_pays_the_wager_on_top_of_base_reward(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        tunnel = _setup_pinnacle(dig_repo, player_repository, guild_id)
        before = player_repository.get_balance(20001, guild_id)

        # bold tier, win chance below the taper knee -> full BOSS_PAYOUTS[350]
        # bold multiplier (3.8); a 200 wager profits int(200 * (3.8 - 1)) = 560.
        result = _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=3, won=True, wager=200, risk_tier="bold", win_chance=0.55,
        )

        assert result["payout"] == PINNACLE_BASE_JC_REWARD + 560
        assert (player_repository.get_balance(20001, guild_id)
                == before + PINNACLE_BASE_JC_REWARD + 560)

    def test_full_clear_wager_payout_tapers_at_high_win_chance(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        tunnel = _setup_pinnacle(dig_repo, player_repository, guild_id)
        before = player_repository.get_balance(20001, guild_id)

        # Win chance at the cap -> fair odds ~1/0.95; a 200 wager returns only
        # int(200 * (1/0.95 - 1)) = 10. Softening then betting big is dead.
        result = _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=3, won=True, wager=200, risk_tier="bold", win_chance=0.95,
        )

        assert result["payout"] == PINNACLE_BASE_JC_REWARD + 10
        assert (player_repository.get_balance(20001, guild_id)
                == before + PINNACLE_BASE_JC_REWARD + 10)

    def test_full_clear_without_a_wager_pays_only_the_base_reward(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        tunnel = _setup_pinnacle(dig_repo, player_repository, guild_id)
        before = player_repository.get_balance(20001, guild_id)

        result = _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel, phase_idx=3, won=True, wager=0,
        )

        assert result["payout"] == PINNACLE_BASE_JC_REWARD
        assert (player_repository.get_balance(20001, guild_id)
                == before + PINNACLE_BASE_JC_REWARD)

    def test_phase_loss_forfeits_the_wager(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        tunnel = _setup_pinnacle(dig_repo, player_repository, guild_id)
        before = player_repository.get_balance(20001, guild_id)

        _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=2, won=False, wager=200,
        )

        assert player_repository.get_balance(20001, guild_id) == before - 200

    def test_phase1_win_carries_the_wager_forward(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        tunnel = _setup_pinnacle(dig_repo, player_repository, guild_id)

        _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=1, won=True, wager=150, risk_tier="reckless",
        )

        entry = json.loads(
            dig_repo.get_tunnel(20001, guild_id)["boss_progress"]
        )[str(PINNACLE_DEPTH)]
        assert entry["carried_wager"] == 150
        assert entry["carried_risk_tier"] == "reckless"

    def test_phase_loss_clears_a_carried_wager(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        tunnel = _setup_pinnacle(dig_repo, player_repository, guild_id)
        boss_progress = {
            str(PINNACLE_DEPTH): {
                "status": "phase1_defeated", "boss_id": "forgotten_king",
                "carried_wager": 200, "carried_risk_tier": "bold",
            }
        }

        _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=2, won=False, wager=200, boss_progress=boss_progress,
        )

        entry = json.loads(
            dig_repo.get_tunnel(20001, guild_id)["boss_progress"]
        )[str(PINNACLE_DEPTH)]
        assert "carried_wager" not in entry
        assert "carried_risk_tier" not in entry


def _read_carry(dig_repo, guild_id, discord_id=20001):
    """Read the carried wager off the persisted tunnel, mimicking what the
    command layer does between phases to size the next fight's stake."""
    tunnel = dict(dig_repo.get_tunnel(discord_id, guild_id))
    bp = json.loads(tunnel["boss_progress"]) if tunnel.get("boss_progress") else {}
    entry = bp.get(str(PINNACLE_DEPTH), {})
    return tunnel, bp, entry.get("carried_wager", 0), entry.get("carried_risk_tier")


class TestWagerSettledExactlyOnceAcrossPhases:
    """End-to-end: a wager placed in phase 1 rides phases 2 and 3 and is
    settled exactly once over the full chain.

    The isolated single-phase tests above each pin one branch; these chain all
    three ``_finalize_pinnacle_outcome`` calls — re-reading the carried wager
    from the persisted tunnel between phases, exactly as the command layer
    does — and assert the player's balance moves by the wager (or its profit)
    on precisely one phase, never on the intermediate wins and never twice.
    """

    def test_three_phase_full_clear_pays_wager_profit_once(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        tunnel = _setup_pinnacle(dig_repo, player_repository, guild_id)
        start_balance = player_repository.get_balance(20001, guild_id)

        # --- Phase 1 win: carry is set, balance MUST NOT move yet. ---
        r1 = _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=1, won=True, wager=200, risk_tier="bold", win_chance=0.55,
        )
        assert r1["payout"] == 0
        assert player_repository.get_balance(20001, guild_id) == start_balance, (
            "Phase 1 win must not pay or charge the wager"
        )

        # --- Phase 2 win: carry rides, balance still unchanged. ---
        tunnel, bp, carried, carried_tier = _read_carry(dig_repo, guild_id)
        assert carried == 200 and carried_tier == "bold"
        r2 = _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=2, won=True, wager=carried, risk_tier=carried_tier,
            win_chance=0.55, boss_progress=bp,
        )
        assert r2["payout"] == 0
        assert player_repository.get_balance(20001, guild_id) == start_balance, (
            "Phase 2 win must not pay or charge the wager"
        )

        # --- Phase 3 win: the carried wager pays out exactly once. ---
        tunnel, bp, carried, carried_tier = _read_carry(dig_repo, guild_id)
        assert carried == 200 and carried_tier == "bold"
        r3 = _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=3, won=True, wager=carried, risk_tier=carried_tier,
            win_chance=0.55, boss_progress=bp,
        )
        # bold mult 3.8 untapered -> profit int(200 * 2.8) = 560.
        assert r3["wager_payout"] == 560
        assert r3["payout"] == PINNACLE_BASE_JC_REWARD + 560
        assert player_repository.get_balance(20001, guild_id) == (
            start_balance + PINNACLE_BASE_JC_REWARD + 560
        ), "Full clear pays base + wager profit exactly once across 3 phases"

        # Carry markers are gone after the final settlement.
        _, _, carried_after, _ = _read_carry(dig_repo, guild_id)
        assert carried_after == 0

    def test_phase2_loss_debits_wager_once_after_phase1_carry(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        tunnel = _setup_pinnacle(dig_repo, player_repository, guild_id)
        start_balance = player_repository.get_balance(20001, guild_id)

        # Phase 1 win carries the wager — no balance change.
        _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=1, won=True, wager=200, risk_tier="bold", win_chance=0.55,
        )
        assert player_repository.get_balance(20001, guild_id) == start_balance

        # Phase 2 loss: the carried stake is debited exactly once.
        tunnel, bp, carried, carried_tier = _read_carry(dig_repo, guild_id)
        assert carried == 200
        _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=2, won=False, wager=carried, risk_tier=carried_tier,
            win_chance=0.55, boss_progress=bp,
        )
        assert player_repository.get_balance(20001, guild_id) == start_balance - 200, (
            "Phase 2 loss debits the carried wager once — stake never double-charged"
        )

        # Carry is cleared so a later finalize cannot debit it again.
        _, _, carried_after, _ = _read_carry(dig_repo, guild_id)
        assert carried_after == 0

    def test_phase3_loss_after_two_carries_debits_wager_once(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        tunnel = _setup_pinnacle(dig_repo, player_repository, guild_id)
        start_balance = player_repository.get_balance(20001, guild_id)

        # Phase 1 + 2 wins both carry forward with no balance movement.
        _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=1, won=True, wager=150, risk_tier="reckless", win_chance=0.55,
        )
        tunnel, bp, carried, carried_tier = _read_carry(dig_repo, guild_id)
        _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=2, won=True, wager=carried, risk_tier=carried_tier,
            win_chance=0.55, boss_progress=bp,
        )
        assert player_repository.get_balance(20001, guild_id) == start_balance, (
            "Two carried wins must not move the balance"
        )

        # Phase 3 loss forfeits the wager once — the full ride is settled here.
        tunnel, bp, carried, carried_tier = _read_carry(dig_repo, guild_id)
        assert carried == 150
        _finalize_pinnacle(
            dig_service, guild_id, tunnel=tunnel,
            phase_idx=3, won=False, wager=carried, risk_tier=carried_tier,
            win_chance=0.55, boss_progress=bp,
        )
        assert player_repository.get_balance(20001, guild_id) == start_balance - 150, (
            "Phase 3 loss debits the carried wager exactly once"
        )
