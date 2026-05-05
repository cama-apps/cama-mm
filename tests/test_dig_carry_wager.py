"""Multi-phase boss fight: wager + risk_tier carry forward across phases.

Covers the headline behavior change in this patch — phase 1 victory no longer
forfeits the wager. The original stake rides through phase 2/3 and is settled
on full defeat. Loss or retreat between phases drops the carry markers.
"""

import json

import pytest

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
