"""Command-level tests for /mana display and claim edge cases.

Covers:
- get_current_mana gating Guardian fields on today's assignment (stale rows)
- the single-player embed hiding Guardian Aura for stale Plains rows
- the double-tap claim race surfacing a friendly message instead of an error
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

import commands.mana as mana_commands
from repositories.mana_repository import ManaRepository
from repositories.player_repository import PlayerRepository
from services.mana_service import ManaService, get_today_pst
from tests.conftest import TEST_GUILD_ID

GID = TEST_GUILD_ID


def _make_mana_service(mana_repo, player_repo):
    gambling_stats = MagicMock()
    # Real numbers: calculate_land_weights compares these with >=.
    gambling_stats.calculate_degen_score.return_value = SimpleNamespace(
        total=0,
        max_leverage_score=0,
        loss_chase_score=0,
        bet_size_score=0,
        negative_loan_bonus=0,
        bankruptcy_score=0,
        debt_depth_score=0,
    )
    gambling_stats.get_player_bet_outcomes.return_value = []
    bankruptcy_service = MagicMock()
    bankruptcy_service.get_state.return_value = MagicMock(
        penalty_games_remaining=0, last_bankruptcy_at=None
    )
    tip_repo = MagicMock()
    tip_repo.get_user_tip_stats.return_value = {"total_sent": 0, "tips_sent_count": 0}
    return ManaService(
        mana_repo=mana_repo,
        player_repo=player_repo,
        gambling_stats_service=gambling_stats,
        bankruptcy_service=bankruptcy_service,
        tip_repo=tip_repo,
    )


@pytest.fixture
def mana_repo(repo_db_path):
    return ManaRepository(repo_db_path)


@pytest.fixture
def mana_service(mana_repo, repo_db_path):
    return _make_mana_service(mana_repo, PlayerRepository(repo_db_path))


class TestStaleGuardianDisplay:
    """A Plains row from a previous day must not present an active shield."""

    def test_stale_plains_row_has_no_guardian_fields(self, mana_repo, mana_service):
        assert mana_repo.claim_mana_atomic(501, GID, "Plains", "2020-01-01")
        current = mana_service.get_current_mana(501, GID)
        assert current is not None
        assert current["land"] == "Plains"
        assert current["guardian_remaining"] == 0
        assert current["consumed"] is False

    def test_todays_plains_row_keeps_guardian_fields(self, mana_repo, mana_service):
        assert mana_repo.claim_mana_atomic(502, GID, "Plains", get_today_pst())
        current = mana_service.get_current_mana(502, GID)
        assert current is not None
        assert current["guardian_remaining"] == 25

    def test_stale_plains_embed_hides_guardian_aura(self):
        member = SimpleNamespace(display_name="P", display_avatar=None)
        mana = {
            "land": "Plains",
            "assigned_date": "2020-01-01",
            "guardian_remaining": 25,
            "consumed": False,
        }
        embed = mana_commands._build_single_embed(member, mana)
        assert all("Guardian" not in field.name for field in embed.fields)

    def test_todays_plains_embed_shows_guardian_aura(self):
        member = SimpleNamespace(display_name="P", display_avatar=None)
        mana = {
            "land": "Plains",
            "assigned_date": get_today_pst(),
            "guardian_remaining": 25,
            "consumed": False,
        }
        embed = mana_commands._build_single_embed(member, mana)
        assert any("Guardian" in field.name for field in embed.fields)


class TestDoubleTapClaimRace:
    """A lost claim_mana_atomic race must answer politely, not error out."""

    async def test_lost_race_reports_already_claimed(
        self, mana_repo, mana_service, monkeypatch
    ):
        # The "other" tap already landed today's claim.
        assert mana_repo.claim_mana_atomic(601, GID, "Forest", get_today_pst())
        # Simulate the race window: this tap's pre-check saw no claim yet.
        monkeypatch.setattr(mana_service, "has_assigned_today", lambda *a, **k: False)

        monkeypatch.setattr(
            mana_commands, "require_gamba_channel", AsyncMock(return_value=True)
        )
        monkeypatch.setattr(mana_commands, "safe_defer", AsyncMock(return_value=True))
        safe_followup = AsyncMock()
        monkeypatch.setattr(mana_commands, "safe_followup", safe_followup)

        client = SimpleNamespace(mana_service=mana_service, mana_effects_service=None)
        interaction = SimpleNamespace(
            guild=SimpleNamespace(id=GID, members=[]),
            user=SimpleNamespace(id=601, roles=[], display_name="Racer"),
            client=client,
            channel=None,
        )
        cog = mana_commands.ManaCommands(SimpleNamespace())

        await cog.mana.callback(cog, interaction, None, False)

        call = safe_followup.await_args
        assert call is not None
        assert "already claimed" in call.kwargs["content"]
        assert call.kwargs["ephemeral"] is True


def test_fresh_plains_stipends_only_process_batch_claim_winners():
    effects = MagicMock()
    effects.apply_bankrupt_stipends.return_value = {1: 5, 3: 0}

    paid = mana_commands._apply_fresh_plains_stipends(
        effects,
        [
            {"discord_id": 1, "land": "Plains"},
            {"discord_id": 2, "land": "Forest"},
            {"discord_id": 3, "land": "Plains"},
        ],
        GID,
    )

    effects.apply_bankrupt_stipends.assert_called_once_with([1, 3], GID)
    assert paid == {1: 5, 3: 0}
