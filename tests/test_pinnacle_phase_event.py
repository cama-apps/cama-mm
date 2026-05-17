"""Tests for pinnacle phase-transition event effects.

A phase-transition event drawn when a pinnacle boss enters its next phase
carries a ``boss_hp_delta``. That delta must reach the next phase's fight —
the pinnacle flow applies it the same way the regular multi-phase boss flow
does, by consuming ``pending_phase_event_id`` when the phase fight starts.
"""

from __future__ import annotations

import json
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    BOSS_BOUNDARIES,
    PINNACLE_DEPTH,
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


def _register(player_repo, discord_id, balance=2000):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"User{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


def _run_pinnacle_phase2_loss(
    dig_service, dig_repo, player_repository, monkeypatch,
    *, discord_id, pending_event_id,
) -> int:
    """Drive a deterministic phase-2 pinnacle loss and return the phase's
    starting boss HP (``boss_hp_max`` on the loss result).

    ``pending_event_id`` is stashed on the PINNACLE_DEPTH boss_progress entry
    exactly as a phase-1 win would leave it; pass ``None`` for the control.
    The fight is forced to a no-chip loss (player misses every swing) so the
    reported ``boss_hp_max`` is the untouched phase-2 starting HP. A distinct
    ``discord_id`` per run keeps the two runs independent in the shared DB.
    """
    _register(player_repository, discord_id)
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    monkeypatch.setattr(random, "random", lambda: 0.99)
    dig_service.dig(discord_id, TEST_GUILD_ID)

    # All tier bosses cleared; the pinnacle entry is mid-fight at phase 2.
    bp: dict = {str(b): "defeated" for b in BOSS_BOUNDARIES}
    pin_entry: dict = {
        "status": "phase1_defeated",
        "boss_id": "forgotten_king",
        "first_meet_seen": True,
    }
    if pending_event_id is not None:
        pin_entry["pending_phase_event_id"] = pending_event_id
    bp[str(PINNACLE_DEPTH)] = pin_entry
    dig_repo.update_tunnel(
        discord_id, TEST_GUILD_ID,
        depth=PINNACLE_DEPTH - 1,
        boss_progress=json.dumps(bp),
        prestige_level=0,
        pinnacle_boss_id="forgotten_king",
        pinnacle_phase=2,
    )

    # Testing HP carry-over, not the mechanic prompt — disable it.
    import domain.models.boss_mechanics as _bm
    monkeypatch.setattr(_bm, "get_mechanic", lambda mid: None)
    # reckless + every roll a miss for the player → guaranteed no-chip loss.
    monkeypatch.setattr(random, "random", lambda: 0.999)

    result = dig_service.fight_boss(discord_id, TEST_GUILD_ID, "reckless", wager=0)
    assert result["success"]
    assert result["won"] is False
    assert result["boundary"] == PINNACLE_DEPTH
    return int(result["boss_hp_max"])


class TestPinnaclePhaseEventHpDelta:
    """A phase-transition event's boss_hp_delta must change the next
    pinnacle phase's starting HP, mirroring the regular boss flow."""

    def test_negative_boss_hp_delta_wounds_next_phase(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        # void_pull carries boss_hp_delta=-2.
        with_event = _run_pinnacle_phase2_loss(
            dig_service, dig_repo, player_repository, monkeypatch,
            discord_id=10001, pending_event_id="void_pull",
        )
        control = _run_pinnacle_phase2_loss(
            dig_service, dig_repo, player_repository, monkeypatch,
            discord_id=10002, pending_event_id=None,
        )
        # The pinnacle's phase-2 boss starts 2 HP lighter because the
        # transition event's boss_hp_delta was applied to its fresh HP.
        assert with_event == control - 2

    def test_zero_boss_hp_delta_leaves_next_phase_unchanged(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        # quiet carries boss_hp_delta=0 — must not move the starting HP.
        with_event = _run_pinnacle_phase2_loss(
            dig_service, dig_repo, player_repository, monkeypatch,
            discord_id=10001, pending_event_id="quiet",
        )
        control = _run_pinnacle_phase2_loss(
            dig_service, dig_repo, player_repository, monkeypatch,
            discord_id=10002, pending_event_id=None,
        )
        assert with_event == control
