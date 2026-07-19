"""
Tests for Immortal Draft functionality.
"""

import asyncio
import inspect
from datetime import datetime
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.draft import DraftCommands
from commands.match import MatchCommands
from domain.models.draft import DraftPhase, DraftState
from domain.models.lobby import Lobby
from domain.models.pending_match_state import PendingMatchState
from domain.services.draft_service import DraftService
from repositories.player_repository import PlayerRepository
from services.draft_state_manager import DraftStateManager
from tests.conftest import TEST_GUILD_ID


def test_captain_opt_in_command_is_removed():
    """Immortal Draft captains are selected automatically, without an opt-in command."""
    assert not hasattr(DraftCommands, "setcaptain")


def test_fake_player_commands_do_not_expose_captain_opt_in():
    """Testing helpers should mirror the production no-opt-in captain flow."""
    from commands.admin import AdminCommands

    assert "captain_eligible" not in inspect.signature(AdminCommands.addfake.callback).parameters
    assert "captain_eligible" not in inspect.signature(
        AdminCommands.filllobbytest.callback
    ).parameters


class TestDraftState:
    """Tests for DraftState domain model."""

    def test_initial_state(self):
        """New DraftState has correct defaults."""
        state = DraftState(guild_id=123)
        assert state.guild_id == 123
        assert state.phase == DraftPhase.COINFLIP
        assert state.player_pool_ids == []
        assert state.captain1_id is None
        assert state.captain2_id is None
        assert state.current_pick_index == 0

    def test_available_player_ids(self):
        """Available players excludes picked players."""
        state = DraftState(guild_id=123)
        state.player_pool_ids = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        state.radiant_player_ids = [1, 2]
        state.dire_player_ids = [3]

        available = state.available_player_ids
        assert 1 not in available
        assert 2 not in available
        assert 3 not in available
        assert 4 in available
        assert len(available) == 7

    def test_available_player_ids_excludes_captains(self):
        """Captains are excluded from available players even if not yet in team lists."""
        state = DraftState(guild_id=123)
        state.player_pool_ids = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        # Set captains but don't add them to team lists yet
        state.radiant_captain_id = 1
        state.dire_captain_id = 2
        state.radiant_player_ids = []
        state.dire_player_ids = []

        available = state.available_player_ids
        assert 1 not in available  # Radiant captain excluded
        assert 2 not in available  # Dire captain excluded
        assert 3 in available
        assert len(available) == 8  # 10 pool - 2 captains = 8 draftable

    def test_current_captain_id_during_drafting(self):
        """Current captain ID is correct during drafting phase."""
        state = DraftState(guild_id=123)
        state.phase = DraftPhase.DRAFTING
        state.radiant_captain_id = 100
        state.dire_captain_id = 200
        state.current_round_first_captain_id = 100

        # Pick 0: first captain (Radiant)
        state.current_pick_index = 0
        assert state.current_captain_id == 100

        # Pick 1: the other captain (Dire)
        state.current_pick_index = 1
        assert state.current_captain_id == 200

    def test_current_captain_id_not_drafting(self):
        """Current captain ID is None when not in drafting phase."""
        state = DraftState(guild_id=123)
        state.phase = DraftPhase.COINFLIP
        assert state.current_captain_id is None

    def test_picks_remaining_this_turn(self):
        """Picks remaining correctly counts consecutive picks."""
        state = DraftState(guild_id=123)
        state.phase = DraftPhase.DRAFTING
        state.radiant_captain_id = 100
        state.dire_captain_id = 200
        state.current_round_first_captain_id = 100

        # Pick 0: Radiant has 1 pick
        state.current_pick_index = 0
        assert state.picks_remaining_this_turn == 1

        # Pick 1: Dire also has exactly 1 pick
        state.current_pick_index = 1
        assert state.picks_remaining_this_turn == 1

        # Every later pick is also a single-pick turn
        state.current_pick_index = 3
        assert state.picks_remaining_this_turn == 1

    def test_pick_player_success(self):
        """Picking a player adds them to correct team."""
        state = DraftState(guild_id=123)
        state.phase = DraftPhase.DRAFTING
        state.player_pool_ids = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        state.radiant_captain_id = 100
        state.dire_captain_id = 200
        state.current_round_first_captain_id = 100

        # First pick goes to Radiant
        result = state.pick_player(5)
        assert result is True
        assert 5 in state.radiant_player_ids
        assert state.current_pick_index == 1

    def test_pick_player_invalid(self):
        """Cannot pick player not in available pool."""
        state = DraftState(guild_id=123)
        state.phase = DraftPhase.DRAFTING
        state.player_pool_ids = [1, 2, 3]
        state.radiant_captain_id = 100
        state.dire_captain_id = 200
        state.current_round_first_captain_id = 100

        result = state.pick_player(999)  # Not in pool
        assert result is False

    def test_pick_player_rejects_wrong_picker(self):
        """pick_player(picker_id=...) refuses a captain who isn't on the clock.

        This is the synchronous turn guard that closes the concurrent-pick race
        (finding 1): two button callbacks from the same captain both pass the
        async pre-defer turn check, then both reach pick_player. The first
        advances the round order; the second must be rejected here because it is
        no longer that captain's turn.
        """
        state = DraftState(guild_id=123)
        state.phase = DraftPhase.DRAFTING
        state.player_pool_ids = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        state.radiant_captain_id = 100
        state.dire_captain_id = 200
        state.current_round_first_captain_id = 100

        # Index 0 belongs to captain 100. Captain 200 may not pick out of turn.
        assert state.current_captain_id == 100
        assert state.pick_player(5, picker_id=200) is False
        assert 5 not in state.dire_player_ids
        assert state.current_pick_index == 0  # nothing advanced

        # The captain on the clock succeeds, which advances to captain 200's turn.
        assert state.pick_player(5, picker_id=100) is True
        assert state.current_pick_index == 1
        assert state.current_captain_id == 200

        # A stale second click from captain 100 (the race) is now rejected: it is
        # no longer their turn, so the pick cannot land on the wrong team.
        assert state.pick_player(6, picker_id=100) is False
        assert 6 not in state.radiant_player_ids
        assert state.current_pick_index == 1

    def test_draft_complete(self):
        """Draft is complete after 8 picks."""
        state = DraftState(guild_id=123)
        state.current_pick_index = 7
        assert state.is_draft_complete is False

        state.current_pick_index = 8
        assert state.is_draft_complete is True

    def test_set_side_preference(self):
        """Side preference can be set for available players."""
        state = DraftState(guild_id=123)
        state.player_pool_ids = [1, 2, 3]

        result = state.set_side_preference(1, "radiant")
        assert result is True
        assert state.side_preferences[1] == "radiant"

        # Clear preference
        result = state.set_side_preference(1, None)
        assert result is True
        assert 1 not in state.side_preferences

    def test_to_dict_and_from_dict(self):
        """State can be serialized and deserialized."""
        state = DraftState(guild_id=123)
        state.captain1_id = 100
        state.captain2_id = 200
        state.phase = DraftPhase.DRAFTING
        state.player_pool_ids = [1, 2, 3]

        data = state.to_dict()
        restored = DraftState.from_dict(data)

        assert restored.guild_id == 123
        assert restored.captain1_id == 100
        assert restored.captain2_id == 200
        assert restored.phase == DraftPhase.DRAFTING
        assert restored.player_pool_ids == [1, 2, 3]

    def test_exclusion_update_metadata_survives_round_trip(self):
        state = DraftState(guild_id=123)
        state.full_exclusion_increment_ids = [11, 12]
        state.half_exclusion_increment_ids = [13]

        restored = DraftState.from_dict(state.to_dict())

        assert restored.full_exclusion_increment_ids == [11, 12]
        assert restored.half_exclusion_increment_ids == [13]

    def test_pending_exclusion_update_metadata_survives_round_trip(self):
        state = PendingMatchState(
            exclusion_updates_deferred=True,
            full_exclusion_increment_ids=[11, 12],
            half_exclusion_increment_ids=[13],
        )

        restored = PendingMatchState.from_dict(state.to_dict())

        assert restored.exclusion_updates_deferred is True
        assert restored.full_exclusion_increment_ids == [11, 12]
        assert restored.half_exclusion_increment_ids == [13]

    def test_player_pool_data_serialization(self):
        """Player pool data is correctly serialized and deserialized."""
        state = DraftState(guild_id=123)
        state.player_pool_ids = [1, 2, 3]
        state.player_pool_data = {
            1: {"name": "Alice", "rating": 1800.0, "roles": ["1", "2"]},
            2: {"name": "Bob", "rating": 1650.0, "roles": ["3"]},
            3: {"name": "Charlie", "rating": 1500.0, "roles": ["4", "5"]},
        }

        data = state.to_dict()
        assert "player_pool_data" in data
        assert data["player_pool_data"][1]["name"] == "Alice"

        restored = DraftState.from_dict(data)
        assert restored.player_pool_data == state.player_pool_data
        assert restored.player_pool_data[2]["rating"] == 1650.0

    def test_player_pool_data_empty_by_default(self):
        """New DraftState has empty player_pool_data."""
        state = DraftState(guild_id=123)
        assert state.player_pool_data == {}

    def test_player_pool_data_survives_round_trip(self):
        """Player pool data survives to_dict/from_dict with various data types."""
        state = DraftState(guild_id=456)
        state.player_pool_data = {
            100: {"name": "Player100", "rating": 2100.5, "roles": []},
            200: {"name": "Player200", "rating": 1400.0, "roles": ["1", "2", "3", "4", "5"]},
        }

        # Round trip
        restored = DraftState.from_dict(state.to_dict())

        # Verify exact equality
        assert restored.player_pool_data[100]["name"] == "Player100"
        assert restored.player_pool_data[100]["rating"] == 2100.5
        assert restored.player_pool_data[100]["roles"] == []
        assert restored.player_pool_data[200]["roles"] == ["1", "2", "3", "4", "5"]


class TestDraftStateManager:
    """Tests for DraftStateManager."""

    def test_create_draft(self):
        """Can create a new draft state."""
        manager = DraftStateManager()
        state = manager.create_draft(guild_id=123)

        assert state is not None
        assert state.guild_id == 123
        assert manager.has_active_draft(123) is True

    def test_create_draft_already_exists(self):
        """Cannot create draft when one exists."""
        manager = DraftStateManager()
        manager.create_draft(guild_id=123)

        with pytest.raises(ValueError, match="already in progress"):
            manager.create_draft(guild_id=123)

    def test_get_state(self):
        """Can retrieve draft state."""
        manager = DraftStateManager()
        created = manager.create_draft(guild_id=123)

        retrieved = manager.get_state(123)
        assert retrieved is created

    def test_get_state_nonexistent(self):
        """Returns None for nonexistent draft."""
        manager = DraftStateManager()
        assert manager.get_state(999) is None

    def test_clear_state(self):
        """Can clear draft state."""
        manager = DraftStateManager()
        manager.create_draft(guild_id=123)

        cleared = manager.clear_state(123)
        assert cleared is not None
        assert manager.get_state(123) is None
        assert manager.has_active_draft(123) is False

    def test_has_active_draft_complete(self):
        """Completed draft is not active."""
        manager = DraftStateManager()
        state = manager.create_draft(guild_id=123)
        state.phase = DraftPhase.COMPLETE

        assert manager.has_active_draft(123) is False

    def test_create_draft_clears_stale_complete_state(self):
        """create_draft succeeds when a stale COMPLETE state exists."""
        manager = DraftStateManager()
        old_state = manager.create_draft(guild_id=123)
        old_state.phase = DraftPhase.COMPLETE

        # Should NOT raise — clears the stale COMPLETE state and creates a new one
        new_state = manager.create_draft(guild_id=123)
        assert new_state is not old_state
        assert new_state.phase == DraftPhase.COINFLIP
        assert manager.get_state(123) is new_state

    def test_create_draft_rejects_active_state(self):
        """create_draft raises when a non-COMPLETE state exists."""
        manager = DraftStateManager()
        state = manager.create_draft(guild_id=123)
        state.phase = DraftPhase.DRAFTING

        with pytest.raises(ValueError, match="already in progress"):
            manager.create_draft(guild_id=123)

    def test_clear_after_create_allows_new_draft(self):
        """Simulates _execute_draft cleanup: create then clear on failure allows retry."""
        manager = DraftStateManager()
        state = manager.create_draft(guild_id=123)
        state.phase = DraftPhase.WINNER_CHOICE

        # Simulate failure cleanup (what _execute_draft now does)
        manager.clear_state(123)
        assert manager.has_active_draft(123) is False

        # Should be able to create a new draft
        new_state = manager.create_draft(guild_id=123)
        assert new_state is not state
        assert new_state.phase == DraftPhase.COINFLIP

    def test_advance_phase(self):
        """Can advance draft phase."""
        manager = DraftStateManager()
        manager.create_draft(guild_id=123)

        result = manager.advance_phase(123, DraftPhase.WINNER_CHOICE)
        assert result is True

        state = manager.get_state(123)
        assert state.phase == DraftPhase.WINNER_CHOICE

    def test_guild_id_normalization(self):
        """None guild_id is normalized to 0."""
        manager = DraftStateManager()
        state = manager.create_draft(guild_id=None)

        assert manager.get_state(None) is state
        assert manager.get_state(0) is state


class TestDraftService:
    """Tests for DraftService domain logic."""

    def test_select_captains_both_specified(self):
        """When both captains specified, use them directly."""
        service = DraftService()
        ratings = {100: 1500.0, 200: 1600.0}

        result = service.select_captains(
            player_pool_ids=[100, 200, 300],
            player_ratings=ratings,
            specified_captain1=100,
            specified_captain2=200,
        )

        assert result.captain1_id == 100
        assert result.captain2_id == 200
        assert result.captain1_rating == 1500.0
        assert result.captain2_rating == 1600.0

    def test_select_captains_not_enough_players(self):
        """Raises an error when the final pool cannot supply two captains."""
        service = DraftService()
        ratings = {100: 1500.0}

        with pytest.raises(ValueError, match="at least 2"):
            service.select_captains(
                player_pool_ids=[100],
                player_ratings=ratings,
            )

    def test_select_captains_automatic_selection(self):
        """When neither specified, selects two distinct captains."""
        service = DraftService()
        ratings = {100: 1500.0, 200: 1500.0, 300: 1500.0}

        result = service.select_captains(
            player_pool_ids=[100, 200, 300],
            player_ratings=ratings,
        )

        assert result.captain1_id in [100, 200, 300]
        assert result.captain2_id in [100, 200, 300]
        assert result.captain1_id != result.captain2_id

    def test_select_captains_chooses_closest_glicko_pair_without_randomness(self, monkeypatch):
        """When neither captain is specified, choose the closest Glicko pair deterministically."""
        service = DraftService()
        ratings = {100: 1500.0, 200: 1795.0, 300: 1800.0, 400: 2200.0}

        def fail_random(*args, **kwargs):
            raise AssertionError("captain selection should not use randomness")

        monkeypatch.setattr("domain.services.draft_service.random.choices", fail_random)
        monkeypatch.setattr("domain.services.draft_service.random.random", fail_random)

        result = service.select_captains(
            player_pool_ids=[100, 200, 300, 400],
            player_ratings=ratings,
        )

        assert {result.captain1_id, result.captain2_id} == {200, 300}
        assert abs(result.captain1_rating - result.captain2_rating) == 5.0

    def test_select_captains_with_specified_captain_chooses_closest_rating(self):
        """When one captain is specified, select the closest-rated pool member."""
        service = DraftService()
        # Captain 100 at 1500, captain 200 at 1500, captain 300 at 2000
        ratings = {100: 1500.0, 200: 1500.0, 300: 2000.0}

        result = service.select_captains(
            player_pool_ids=[100, 200, 300],
            player_ratings=ratings,
            specified_captain1=100,  # Force captain1 to be 100
        )

        assert result.captain2_id == 200

    def test_select_player_pool_exact_size(self):
        """When lobby equals pool size, all selected."""
        service = DraftService()

        result = service.select_player_pool(
            regular_player_ids=[1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            conditional_player_ids=[],
            exclusion_counts={},
            player_ratings={},
            pool_size=10,
        )

        assert len(result.selected_ids) == 10
        assert result.excluded_ids == []

    def test_select_player_pool_prioritizes_exclusion_factor_over_glicko(self):
        """A higher exclusion factor wins even when its Glicko is lower."""
        service = DraftService()
        ratings = {pid: 2100.0 - pid * 50 for pid in range(1, 12)}
        ratings[11] = 900.0

        result = service.select_player_pool(
            regular_player_ids=list(range(1, 12)),
            conditional_player_ids=[],
            exclusion_counts={11: 1},
            player_ratings=ratings,
            pool_size=10,
        )

        assert 11 in result.selected_ids
        assert 10 in result.excluded_ids

    def test_select_player_pool_uses_glicko_for_equal_exclusion_factors(self):
        """Glicko decides ordering only when exclusion factors are equal."""
        service = DraftService()
        ratings = {pid: 1000.0 + pid * 50 for pid in range(1, 12)}

        result = service.select_player_pool(
            regular_player_ids=list(range(1, 12)),
            conditional_player_ids=[],
            exclusion_counts={},
            player_ratings=ratings,
            pool_size=10,
        )

        assert 1 in result.excluded_ids
        assert set(result.selected_ids) == set(range(2, 12))

    def test_select_player_pool_preserves_lobby_order_for_exact_ties(self):
        """No third tie-breaker reorders players with equal exclusion and Glicko."""
        service = DraftService()
        lobby_order = [9, 3, 7, 1, 8, 2, 6, 4, 5, 10, 11]

        result = service.select_player_pool(
            regular_player_ids=lobby_order,
            conditional_player_ids=[],
            exclusion_counts={},
            player_ratings=dict.fromkeys(lobby_order, 1500.0),
            pool_size=10,
        )

        assert result.selected_ids == lobby_order[:10]
        assert result.excluded_ids == [11]

    def test_select_player_pool_uses_conditionals_only_to_fill_shortage(self):
        """Conditional players cannot displace regular players."""
        service = DraftService()

        result = service.select_player_pool(
            regular_player_ids=list(range(1, 9)),
            conditional_player_ids=[9, 10, 11, 12],
            exclusion_counts={9: 1, 10: 3, 11: 2, 12: 99},
            player_ratings=dict.fromkeys(range(1, 13), 1500.0),
            pool_size=10,
        )

        assert set(range(1, 9)) <= set(result.selected_ids)
        assert {12, 10} <= set(result.selected_ids)
        assert {9, 11} <= set(result.excluded_ids)

    def test_select_player_pool_forces_conditional_captain_into_full_regular_pool(self):
        """A manual captain override displaces the lowest-ranked regular player."""
        service = DraftService()

        result = service.select_player_pool(
            regular_player_ids=list(range(1, 12)),
            conditional_player_ids=[12],
            exclusion_counts={},
            player_ratings={pid: 2200.0 - pid * 50 for pid in range(1, 13)},
            forced_include_ids=[12],
            pool_size=10,
        )

        assert 12 in result.selected_ids
        assert {10, 11} <= set(result.excluded_ids)

    def test_select_player_pool_not_enough(self):
        """Raises error when lobby smaller than pool size."""
        service = DraftService()

        with pytest.raises(ValueError, match="Need at least"):
            service.select_player_pool(
                regular_player_ids=[1, 2, 3],
                conditional_player_ids=[],
                exclusion_counts={},
                player_ratings={},
                pool_size=10,
            )

    def test_coinflip(self):
        """Coinflip returns one of the two captains."""
        service = DraftService()

        results = set()
        for _ in range(100):
            result = service.coinflip(100, 200)
            results.add(result)

        # Should have both outcomes
        assert results == {100, 200}

class TestDynamicDraftRounds:
    """Tests for four two-pick rounds ordered by live team Glicko totals."""

    @staticmethod
    def _make_state(radiant_rating: float, dire_rating: float) -> DraftState:
        state = DraftState(guild_id=123)
        state.captain1_id = 1
        state.captain2_id = 2
        state.captain1_rating = radiant_rating
        state.captain2_rating = dire_rating
        state.radiant_captain_id = 1
        state.dire_captain_id = 2
        state.player_pool_ids = list(range(1, 11))
        state.player_pool_data = {
            1: {"rating": radiant_rating},
            2: {"rating": dire_rating},
            **{pid: {"rating": 1500.0} for pid in range(3, 11)},
        }
        return state

    def test_lower_total_team_picks_first_and_other_team_picks_second(self):
        state = self._make_state(radiant_rating=1400.0, dire_rating=1600.0)

        state.start_player_draft()

        assert state.current_captain_id == 1
        assert state.pick_player(3, picker_id=1) is True
        assert state.current_captain_id == 2

    def test_team_totals_are_recalculated_before_each_round(self):
        state = self._make_state(radiant_rating=1400.0, dire_rating=1600.0)
        state.player_pool_data[3]["rating"] = 600.0
        state.player_pool_data[4]["rating"] = 100.0
        state.start_player_draft()

        assert state.pick_player(3, picker_id=1) is True
        assert state.pick_player(4, picker_id=2) is True

        assert state.radiant_rating_total == 2000.0
        assert state.dire_rating_total == 1700.0
        assert state.current_captain_id == 2

    def test_tied_later_round_starts_with_previous_round_second_picker(self):
        state = self._make_state(radiant_rating=1400.0, dire_rating=1600.0)
        state.player_pool_data[3]["rating"] = 200.0
        state.player_pool_data[4]["rating"] = 0.0
        state.start_player_draft()

        assert state.pick_player(3, picker_id=1) is True
        assert state.pick_player(4, picker_id=2) is True

        assert state.radiant_rating_total == state.dire_rating_total
        assert state.current_captain_id == 2

    def test_tied_first_round_starts_with_coinflip_loser(self):
        state = self._make_state(radiant_rating=1500.0, dire_rating=1500.0)
        state.coinflip_winner_id = 1

        state.start_player_draft()

        assert state.current_captain_id == 2


class TestDynamicDraftPickOrder:
    """Drive complete four-round drafts and assert guild isolation."""

    def _make_state(self, guild_id: int) -> DraftState:
        """Return a DRAFTING-phase state with two captains and 8 draftable players."""
        state = DraftState(guild_id=guild_id)
        state.radiant_captain_id = 1
        state.dire_captain_id = 2
        state.captain1_id = 1
        state.captain2_id = 2
        state.captain1_rating = 1400.0
        state.captain2_rating = 1500.0
        state.player_pool_ids = [1, 2, 11, 12, 13, 14, 15, 16, 17, 18]
        state.player_pool_data = {
            1: {"rating": 1400.0},
            2: {"rating": 1500.0},
            **{pid: {"rating": 1500.0} for pid in range(11, 19)},
        }
        state.start_player_draft()
        return state

    def test_full_dynamic_draft_assigns_four_picks_per_side(self):
        state = self._make_state(guild_id=1001)
        picker_log = []

        for pick_num in range(8):
            picker_log.append(state.current_captain_id)
            player_to_pick = state.available_player_ids[0]
            success = state.pick_player(player_to_pick, picker_id=state.current_captain_id)
            assert success, f"pick_player failed at pick {pick_num}"

        assert state.phase == DraftPhase.COMPLETE
        assert state.current_pick_index == 8
        assert picker_log == [1, 2, 1, 2, 1, 2, 1, 2]
        assert len(state.radiant_player_ids) == 5
        assert len(state.dire_player_ids) == 5
        assert not state.available_player_ids

    def test_guild_isolation_draft_states(self):
        """Two guilds can run independent drafts; picks in one do not affect the other."""
        state_a = self._make_state(guild_id=2001)
        state_b = self._make_state(guild_id=2002)
        state_b.player_pool_ids = [1, 2, 21, 22, 23, 24, 25, 26, 27, 28]
        state_b.player_pool_data.update(
            {pid: {"rating": 1500.0} for pid in range(21, 29)}
        )

        # Pick all 8 players in guild A
        for _ in range(8):
            player = state_a.available_player_ids[0]
            state_a.pick_player(player)

        # Guild B should still be at pick 0 with all 8 players available
        assert state_b.current_pick_index == 0
        assert len(state_b.available_player_ids) == 8
        assert state_b.phase == DraftPhase.DRAFTING

        # Pick all 8 players in guild B
        for _ in range(8):
            player = state_b.available_player_ids[0]
            state_b.pick_player(player)

        assert state_a.phase == DraftPhase.COMPLETE
        assert state_b.phase == DraftPhase.COMPLETE
        # No overlap between teams across guilds
        assert not (
            (set(state_a.radiant_player_ids) - {1, 2})
            & (set(state_b.radiant_player_ids) - {1, 2})
        )


class TestSpecifiedCaptains:
    """Tests for specified captain handling."""

    def test_select_captains_both_specified_from_eligible_pool(self):
        """DraftService.select_captains picks from the final player pool provided."""
        service = DraftService()
        ratings = {1: 1500.0, 2: 1500.0, 3: 1500.0, 4: 1500.0, 5: 1500.0}
        selected = [1, 2]

        result = service.select_captains(
            player_pool_ids=[1, 2, 3, 4, 5],
            player_ratings=ratings,
            specified_captain1=selected[0],
            specified_captain2=selected[1],
        )

        assert result.captain1_id == 1
        assert result.captain2_id == 2

    def test_production_captain_selection_never_duplicates(self):
        """Production captain selection (DraftService.select_captains) never returns the same player twice."""
        service = DraftService()
        player_ids = list(range(1, 17))
        ratings = {pid: 1500.0 + pid * 10 for pid in player_ids}

        for _ in range(50):
            result = service.select_captains(
                player_pool_ids=player_ids,
                player_ratings=ratings,
            )
            assert result.captain1_id != result.captain2_id
            assert result.captain1_id in player_ids
            assert result.captain2_id in player_ids

    def test_shuffle_redirect_threshold(self):
        """The Immortal Draft auto-redirect fires at regular_count >= 15.

        Uses the production Lobby domain model to confirm the boundary:
        14 regular players does NOT meet the >= 15 threshold;
        15 regular players DOES.
        """
        from datetime import datetime

        lobby_below = Lobby(lobby_id=1, created_by=999, created_at=datetime.now())
        for i in range(1, 15):  # 14 regular players
            lobby_below.add_player(i)
        assert lobby_below.get_player_count() < 15, "14 players should be below threshold"

        lobby_at = Lobby(lobby_id=2, created_by=999, created_at=datetime.now())
        for i in range(1, 16):  # 15 regular players
            lobby_at.add_player(i)
        assert lobby_at.get_player_count() >= 15, "15 players should meet threshold"

    def test_lobby_player_count_ignores_legacy_conditional_players(self):
        from datetime import datetime

        from domain.models.lobby import Lobby

        lobby = Lobby(lobby_id=1, created_by=999, created_at=datetime.now())
        # Nine regular players are not enough even if legacy Frogling data exists.
        for i in range(1, 10):
            lobby.add_player(i)
        lobby.conditional_players = set(range(11, 17))

        total = lobby.get_total_count()
        regular = lobby.get_player_count()
        assert total == 9
        assert regular == 9


class TestCaptainEligibility:
    """Tests for captain eligibility repository methods."""

    def test_set_captain_eligible_true(self, player_repository: PlayerRepository):
        """Player can be set as captain-eligible."""
        # Add a player first
        player_repository.add(
            discord_id=1001,
            discord_username="TestPlayer",
            initial_mmr=3000,
            guild_id=TEST_GUILD_ID,
        )

        # Set as captain-eligible
        result = player_repository.set_captain_eligible(1001, TEST_GUILD_ID, True)
        assert result is True

        # Verify eligibility
        assert player_repository.get_captain_eligible(1001, TEST_GUILD_ID) is True

    def test_set_captain_eligible_false(self, player_repository: PlayerRepository):
        """Player can be set as not captain-eligible."""
        # Add a player first
        player_repository.add(
            discord_id=1002,
            discord_username="TestPlayer2",
            initial_mmr=3000,
            guild_id=TEST_GUILD_ID,
        )

        # Set as captain-eligible first
        player_repository.set_captain_eligible(1002, TEST_GUILD_ID, True)
        assert player_repository.get_captain_eligible(1002, TEST_GUILD_ID) is True

        # Remove eligibility
        result = player_repository.set_captain_eligible(1002, TEST_GUILD_ID, False)
        assert result is True
        assert player_repository.get_captain_eligible(1002, TEST_GUILD_ID) is False

    def test_get_captain_eligible_default_false(self, player_repository: PlayerRepository):
        """New players default to not captain-eligible."""
        player_repository.add(
            discord_id=1003,
            discord_username="TestPlayer3",
            initial_mmr=3000,
            guild_id=TEST_GUILD_ID,
        )

        # Should default to False
        assert player_repository.get_captain_eligible(1003, TEST_GUILD_ID) is False

    def test_get_captain_eligible_nonexistent_player(self, player_repository: PlayerRepository):
        """Non-existent player returns False for captain eligibility."""
        assert player_repository.get_captain_eligible(9999, TEST_GUILD_ID) is False

    def test_set_captain_eligible_nonexistent_player(self, player_repository: PlayerRepository):
        """Setting eligibility for non-existent player returns False."""
        result = player_repository.set_captain_eligible(9999, TEST_GUILD_ID, True)
        assert result is False

    def test_get_captain_eligible_players(self, player_repository: PlayerRepository):
        """Get list of captain-eligible players from a set of IDs."""
        # Add several players
        for i in range(1, 6):
            player_repository.add(
                discord_id=2000 + i,
                discord_username=f"Player{i}",
                initial_mmr=3000 + i * 100,
                guild_id=TEST_GUILD_ID,
            )

        # Set some as captain-eligible
        player_repository.set_captain_eligible(2001, TEST_GUILD_ID, True)
        player_repository.set_captain_eligible(2003, TEST_GUILD_ID, True)
        player_repository.set_captain_eligible(2005, TEST_GUILD_ID, True)

        # Query subset of players
        all_ids = [2001, 2002, 2003, 2004, 2005]
        eligible = player_repository.get_captain_eligible_players(all_ids, TEST_GUILD_ID)

        assert sorted(eligible) == [2001, 2003, 2005]

    def test_get_captain_eligible_players_empty_list(self, player_repository: PlayerRepository):
        """Empty input list returns empty result."""
        result = player_repository.get_captain_eligible_players([], TEST_GUILD_ID)
        assert result == []

    def test_get_captain_eligible_players_none_eligible(self, player_repository: PlayerRepository):
        """If no players are eligible, returns empty list."""
        # Add players but don't set any as eligible
        for i in range(1, 4):
            player_repository.add(
                discord_id=3000 + i,
                discord_username=f"Player{i}",
                initial_mmr=3000,
                guild_id=TEST_GUILD_ID,
            )

        eligible = player_repository.get_captain_eligible_players([3001, 3002, 3003], TEST_GUILD_ID)
        assert eligible == []

    def test_get_captain_eligible_players_subset(self, player_repository: PlayerRepository):
        """Only returns eligible players from the requested subset."""
        # Add players
        for i in range(1, 6):
            player_repository.add(
                discord_id=4000 + i,
                discord_username=f"Player{i}",
                initial_mmr=3000,
                guild_id=TEST_GUILD_ID,
            )

        # Set players 1, 2, 3 as eligible
        player_repository.set_captain_eligible(4001, TEST_GUILD_ID, True)
        player_repository.set_captain_eligible(4002, TEST_GUILD_ID, True)
        player_repository.set_captain_eligible(4003, TEST_GUILD_ID, True)

        # Only query for 2 and 4 - should return only 2
        eligible = player_repository.get_captain_eligible_players([4002, 4004], TEST_GUILD_ID)
        assert eligible == [4002]


class TestPlayerPoolVisibility:
    """
    Tests for player pool visibility during pre-draft phases.
    Verifies the cached player data is used correctly without DB queries.
    """

    def test_player_pool_data_excludes_captains(self):
        """Available player IDs correctly excludes captains."""
        state = DraftState(guild_id=123)
        state.player_pool_ids = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        state.captain1_id = 1
        state.captain2_id = 2

        # Simulate what _build_player_pool_field does
        available_ids = [
            pid for pid in state.player_pool_ids
            if pid != state.captain1_id and pid != state.captain2_id
        ]

        assert 1 not in available_ids  # Captain1 excluded
        assert 2 not in available_ids  # Captain2 excluded
        assert len(available_ids) == 8  # 8 draftable players remain

    def test_player_pool_display_sorts_by_rating(self):
        """Player pool display is sorted by rating descending."""
        state = DraftState(guild_id=123)
        state.player_pool_ids = [1, 2, 3, 4, 5]
        state.captain1_id = None
        state.captain2_id = None
        state.player_pool_data = {
            1: {"name": "LowRating", "rating": 1200.0, "roles": ["5"]},
            2: {"name": "HighRating", "rating": 1900.0, "roles": ["1"]},
            3: {"name": "MidRating", "rating": 1500.0, "roles": ["3"]},
            4: {"name": "VeryHighRating", "rating": 2100.0, "roles": ["2"]},
            5: {"name": "VeryLowRating", "rating": 1000.0, "roles": ["4"]},
        }

        # Build player info like _build_player_pool_field does
        player_info = []
        for pid in state.player_pool_ids:
            data = state.player_pool_data.get(pid)
            if data:
                player_info.append({
                    "name": data["name"],
                    "rating": data["rating"],
                    "roles": data["roles"],
                })

        # Sort by rating descending
        player_info.sort(key=lambda p: p["rating"], reverse=True)

        # Verify order
        assert player_info[0]["name"] == "VeryHighRating"
        assert player_info[1]["name"] == "HighRating"
        assert player_info[2]["name"] == "MidRating"
        assert player_info[3]["name"] == "LowRating"
        assert player_info[4]["name"] == "VeryLowRating"

    def test_player_pool_data_fallback_for_missing(self):
        """Missing player data uses fallback values."""
        state = DraftState(guild_id=123)
        state.player_pool_ids = [1, 2, 3]
        state.captain1_id = None
        state.captain2_id = None
        # Only provide data for player 1
        state.player_pool_data = {
            1: {"name": "HasData", "rating": 1800.0, "roles": ["1", "2"]},
        }

        # Build player info like _build_player_pool_field does
        player_info = []
        for pid in state.player_pool_ids:
            data = state.player_pool_data.get(pid)
            if data:
                player_info.append({
                    "name": data["name"],
                    "rating": data["rating"],
                    "roles": data["roles"],
                })
            else:
                player_info.append({
                    "name": f"Player {pid}",
                    "rating": 1500.0,
                    "roles": [],
                })

        # Verify fallback
        assert player_info[0]["name"] == "HasData"
        assert player_info[0]["rating"] == 1800.0
        assert player_info[1]["name"] == "Player 2"  # Fallback
        assert player_info[1]["rating"] == 1500.0  # Default rating
        assert player_info[2]["name"] == "Player 3"
        assert player_info[2]["roles"] == []

    def test_player_pool_empty_when_all_are_captains(self):
        """Returns empty available list when all players are captains."""
        state = DraftState(guild_id=123)
        state.player_pool_ids = [1, 2]  # Only 2 players
        state.captain1_id = 1
        state.captain2_id = 2

        available_ids = [
            pid for pid in state.player_pool_ids
            if pid != state.captain1_id and pid != state.captain2_id
        ]

        assert available_ids == []

    def test_player_pool_data_with_full_draft_state(self):
        """Full draft state integration test with all 10 players."""
        state = DraftState(guild_id=999)

        # Setup 10 players with realistic data
        state.player_pool_ids = list(range(1001, 1011))  # Players 1001-1010
        state.captain1_id = 1001
        state.captain2_id = 1002
        state.captain1_rating = 1850.0
        state.captain2_rating = 1820.0

        # Cache player data for all 10 players
        state.player_pool_data = {
            1001: {"name": "Captain1", "rating": 1850.0, "roles": ["1", "2"]},
            1002: {"name": "Captain2", "rating": 1820.0, "roles": ["2", "3"]},
            1003: {"name": "Player3", "rating": 1750.0, "roles": ["3"]},
            1004: {"name": "Player4", "rating": 1700.0, "roles": ["4", "5"]},
            1005: {"name": "Player5", "rating": 1650.0, "roles": ["5"]},
            1006: {"name": "Player6", "rating": 1600.0, "roles": ["1"]},
            1007: {"name": "Player7", "rating": 1550.0, "roles": ["2"]},
            1008: {"name": "Player8", "rating": 1500.0, "roles": ["3", "4"]},
            1009: {"name": "Player9", "rating": 1450.0, "roles": ["4"]},
            1010: {"name": "Player10", "rating": 1400.0, "roles": ["5"]},
        }

        # Get available (non-captain) players
        available_ids = [
            pid for pid in state.player_pool_ids
            if pid != state.captain1_id and pid != state.captain2_id
        ]

        # Verify 8 players available for draft
        assert len(available_ids) == 8
        assert 1001 not in available_ids  # Captain1 excluded
        assert 1002 not in available_ids  # Captain2 excluded

        # Build sorted player info
        player_info = []
        for pid in available_ids:
            data = state.player_pool_data[pid]
            player_info.append({
                "name": data["name"],
                "rating": data["rating"],
                "roles": data["roles"],
            })
        player_info.sort(key=lambda p: p["rating"], reverse=True)

        # Verify sorting (highest rated first)
        assert player_info[0]["name"] == "Player3"
        assert player_info[0]["rating"] == 1750.0
        assert player_info[-1]["name"] == "Player10"
        assert player_info[-1]["rating"] == 1400.0

        # Verify all 8 players are present
        names = [p["name"] for p in player_info]
        assert "Captain1" not in names
        assert "Captain2" not in names
        assert len(names) == 8

    def test_player_pool_data_preserves_roles(self):
        """Role data is correctly preserved and accessible."""
        state = DraftState(guild_id=123)
        state.player_pool_ids = [1, 2, 3]
        state.player_pool_data = {
            1: {"name": "Carry", "rating": 1800.0, "roles": ["1"]},
            2: {"name": "Flex", "rating": 1750.0, "roles": ["1", "2", "3", "4", "5"]},
            3: {"name": "Support", "rating": 1700.0, "roles": ["4", "5"]},
        }

        assert state.player_pool_data[1]["roles"] == ["1"]
        assert state.player_pool_data[2]["roles"] == ["1", "2", "3", "4", "5"]
        assert state.player_pool_data[3]["roles"] == ["4", "5"]

        # Verify round-trip preserves roles
        restored = DraftState.from_dict(state.to_dict())
        assert restored.player_pool_data[2]["roles"] == ["1", "2", "3", "4", "5"]


# ============================================================================
# Integration tests for _execute_draft (the immortal-draft execution path)
# ============================================================================


class _FakeMessage:
    """A fake Discord message that records edits and deletes."""

    _counter = 0

    def __init__(self, content=None, embed=None, view=None, fail_edit=False, channel=None):
        _FakeMessage._counter += 1
        self.id = 9_000_000 + _FakeMessage._counter
        self.content = content
        self.embed = embed
        self.view = view
        self.channel = channel
        self.jump_url = (
            f"https://discord.test/channels/0/{channel.id}/{self.id}"
            if channel is not None
            else None
        )
        self.edited = False
        self.deleted = False
        self._fail_edit = fail_edit

    async def edit(self, content=None, embed=None, view=None):
        if self._fail_edit:
            raise RuntimeError("simulated Discord edit failure")
        self.edited = True
        self.content = content
        self.embed = embed
        self.view = view
        return self

    async def delete(self):
        self.deleted = True


class _FakeFollowup:
    """Records every followup.send and returns an editable fake message."""

    def __init__(self, fail_edit=False):
        self.messages = []
        self._fail_edit = fail_edit

    async def send(self, content=None, **kwargs):
        msg = _FakeMessage(
            content=content,
            embed=kwargs.get("embed"),
            view=kwargs.get("view"),
            fail_edit=self._fail_edit,
        )
        self.messages.append(msg)
        return msg


class _FakeChannel:
    def __init__(self):
        self.id = 555_000
        self.sent = []

    async def send(self, content=None, **kwargs):
        msg = _FakeMessage(content=content, channel=self)
        self.sent.append(msg)
        return msg


class _FakeGuild:
    def __init__(self, guild_id):
        self.id = guild_id

    def get_member(self, user_id):
        # Force name resolution to fall back to the player repository.
        return None


class _FakeInteraction:
    """Minimal stand-in for an already-deferred discord.Interaction."""

    def __init__(self, guild_id, fail_edit=False):
        self.guild = _FakeGuild(guild_id)
        self.guild_id = guild_id
        self.channel = _FakeChannel()
        self.channel_id = self.channel.id
        self.followup = _FakeFollowup(fail_edit=fail_edit)


class _FakeComponentResponse:
    def __init__(self):
        self.deferred = False
        self.defer_kwargs = None
        self.sent_messages = []
        self.edit_message_calls = []

    def is_done(self):
        return self.deferred or bool(self.sent_messages or self.edit_message_calls)

    async def defer(self, **kwargs):
        self.deferred = True
        self.defer_kwargs = kwargs

    async def send_message(self, content=None, **kwargs):
        self.sent_messages.append({"content": content, **kwargs})

    async def edit_message(self, **kwargs):
        self.edit_message_calls.append(kwargs)
        raise AssertionError("deferred player picks must edit the original response")


class _FakeComponentInteraction(_FakeInteraction):
    def __init__(self, guild_id, user_id):
        super().__init__(guild_id)
        self.user = SimpleNamespace(id=user_id)
        self.response = _FakeComponentResponse()
        self.message = _FakeMessage(channel=self.channel)
        self.edited_original = False

    async def edit_original_response(self, *, embed=None, view=None):
        self.edited_original = True
        self.message.embed = embed
        self.message.view = view
        return self.message

    async def original_response(self):
        return self.message


class _FakeDraftMatchService:
    def __init__(self):
        self.state = None
        self.message_info = None
        self.reserve_calls = []

    def _persist_match_state(self, guild_id, state):
        state.pending_match_id = 1234
        self.state = state

    def reserve_betting_seed(self, guild_id, state):
        self.reserve_calls.append((guild_id, state.pending_match_id))
        return state

    def get_last_shuffle(self, guild_id, pending_match_id=None):
        # Mirror the real service: a specific id only matches its own state
        if (
            pending_match_id is not None
            and self.state is not None
            and self.state.pending_match_id != pending_match_id
        ):
            return None
        return self.state

    def set_last_shuffle(self, guild_id, state):
        self.state = state

    def set_shuffle_message_info(
        self,
        guild_id,
        message_id,
        channel_id,
        jump_url=None,
        thread_message_id=None,
        thread_id=None,
        origin_channel_id=None,
        pending_match_id=None,
        cmd_message_id=None,
        cmd_channel_id=None,
    ):
        """Mirror MatchStateService: same signature, resolve the state by
        pending_match_id (silent no-op on a miss), only overwrite non-None."""
        self.message_info = {
            "message_id": message_id,
            "channel_id": channel_id,
            "jump_url": jump_url,
            "thread_message_id": thread_message_id,
            "thread_id": thread_id,
            "origin_channel_id": origin_channel_id,
            "pending_match_id": pending_match_id,
            "cmd_message_id": cmd_message_id,
            "cmd_channel_id": cmd_channel_id,
        }
        state = self.get_last_shuffle(guild_id, pending_match_id)
        if not state:
            return
        if message_id is not None:
            state.shuffle_message_id = message_id
        if channel_id is not None:
            state.shuffle_channel_id = channel_id
        if jump_url is not None:
            state.shuffle_message_jump_url = jump_url
        if thread_message_id is not None:
            state.thread_shuffle_message_id = thread_message_id
        if thread_id is not None:
            state.thread_shuffle_thread_id = thread_id
        if origin_channel_id is not None:
            state.origin_channel_id = origin_channel_id


_ROLE_CYCLE = [["1"], ["2"], ["3"], ["4"], ["5"], ["1", "2"], ["3", "4"], ["2", "5"]]


def _register_draft_players(player_repo, guild_id, count, *, start_id=50001):
    """Register ``count`` players with varied ratings/roles; return their ids."""
    ids = []
    for i in range(count):
        pid = start_id + i
        player_repo.add(
            discord_id=pid,
            discord_username=f"DraftPlayer{i}",
            guild_id=guild_id,
            initial_mmr=2000 + i * 40,
            preferred_roles=_ROLE_CYCLE[i % len(_ROLE_CYCLE)],
            glicko_rating=1400.0 + i * 30,
            glicko_rd=80.0,
            glicko_volatility=0.06,
        )
        ids.append(pid)
    return ids


def _make_draft_cog(player_repo):
    """Build a DraftCommands cog with real services and a stub bot."""
    return DraftCommands(
        bot=MagicMock(),
        player_repo=player_repo,
        lobby_manager=MagicMock(),
        draft_state_manager=DraftStateManager(),
        draft_service=DraftService(),
        match_service=None,
    )


def _make_lobby(player_ids):
    lobby = Lobby(lobby_id=1, created_by=999, created_at=datetime.now())
    for pid in player_ids:
        lobby.add_player(pid)
    return lobby


class TestExecuteDraft:
    """Integration tests for DraftCommands._execute_draft.

    These drive the real execution path — real PlayerRepository, DraftService
    and DraftStateManager, with a fake Discord interaction — covering the
    immortal-draft flow that broke in production and had no test coverage.
    """

    async def test_completed_pre_draft_choices_start_dynamic_rounds(
        self, player_repository, monkeypatch
    ):
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        captain1, captain2 = player_ids[:2]
        cog = _make_draft_cog(player_repository)
        state = DraftState(guild_id=guild_id)
        state.player_pool_ids = player_ids
        state.player_pool_data = {
            player_id: {"name": f"Player {player_id}", "rating": 1400.0 + index * 30}
            for index, player_id in enumerate(player_ids)
        }
        state.captain1_id = captain1
        state.captain2_id = captain2
        state.captain1_rating = 1400.0
        state.captain2_rating = 1430.0
        state.radiant_captain_id = captain1
        state.dire_captain_id = captain2
        state.coinflip_winner_id = captain1
        state.winner_choice_type = "side"
        state.loser_choice_value = "first"

        interaction = SimpleNamespace(
            guild=_FakeGuild(guild_id),
            channel=_FakeChannel(),
        )
        show_draft_ui = AsyncMock()
        monkeypatch.setattr(cog, "_show_draft_ui", show_draft_ui)
        monkeypatch.setattr("commands.draft.get_neon_service", lambda _bot: None)

        await cog._start_player_draft(interaction, guild_id, state)

        assert state.phase == DraftPhase.DRAFTING
        assert state.radiant_player_ids == [captain1]
        assert state.dire_player_ids == [captain2]
        assert state.current_captain_id == captain1
        show_draft_ui.assert_awaited_once_with(interaction, guild_id, state)

    async def test_draft_embed_shows_round_order_and_team_totals(self, player_repository):
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        captain1, captain2 = player_ids[:2]
        cog = _make_draft_cog(player_repository)
        state = DraftState(guild_id=guild_id)
        state.player_pool_ids = player_ids
        state.player_pool_data = {
            player_id: {
                "name": f"Player {player_id}",
                "rating": 1400.0 + index * 30,
                "roles": [],
            }
            for index, player_id in enumerate(player_ids)
        }
        state.captain1_id = captain1
        state.captain2_id = captain2
        state.captain1_rating = 1400.0
        state.captain2_rating = 1430.0
        state.radiant_captain_id = captain1
        state.dire_captain_id = captain2
        state.radiant_hero_pick_order = 1
        state.dire_hero_pick_order = 2
        state.start_player_draft()

        embed = await cog._build_draft_embed(_FakeGuild(guild_id), state)

        assert "Round 1/4" in embed.description
        assert "Radiant → 🔴 Dire" in embed.description
        assert "🟢 1400" in embed.description
        assert "🔴 1430" in embed.description

    async def test_succeeds_with_sixteen_players(self, player_repository):
        """A full lobby needs no opt-ins and selects captains from the final pool."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 16)
        exclusion_before = player_repository.get_exclusion_counts(player_ids, guild_id)

        cog = _make_draft_cog(player_repository)
        interaction = _FakeInteraction(guild_id)

        result = await cog._execute_draft(interaction, guild_id, _make_lobby(player_ids))

        assert result is True
        state = cog.draft_state_manager.get_state(guild_id)
        assert state is not None
        assert state.phase == DraftPhase.WINNER_CHOICE
        # 2 captains + 8 drafted players
        assert len(state.player_pool_ids) == 10
        assert set(state.player_pool_ids) == set(player_ids[-10:])
        assert {state.captain1_id, state.captain2_id} == set(player_ids[-2:])
        # 16 lobby players - 10 selected = 6 excluded, with no overlap
        assert len(state.excluded_player_ids) == 6
        assert not (set(state.player_pool_ids) & set(state.excluded_player_ids))
        assert state.coinflip_winner_id in (state.captain1_id, state.captain2_id)
        assert set(state.full_exclusion_increment_ids) == set(state.excluded_player_ids)
        assert state.half_exclusion_increment_ids == []
        assert player_repository.get_exclusion_counts(player_ids, guild_id) == exclusion_before
        # the progress message is converted in place into the draft embed
        assert len(interaction.followup.messages) == 1
        draft_msg = interaction.followup.messages[0]
        assert draft_msg.edited is True
        assert draft_msg.embed is not None
        assert draft_msg.view is not None
        assert state.draft_message_id == draft_msg.id
        # both captains were pinged
        assert len(interaction.channel.sent) == 1
        assert "Draft starting!" in interaction.channel.sent[0].content

    async def test_legacy_conditional_players_are_ignored(self, player_repository):
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 16)
        regular_ids = player_ids[:14]
        conditional_ids = player_ids[14:]
        lobby = _make_lobby(regular_ids)
        lobby.conditional_players.update(conditional_ids)
        exclusion_before = player_repository.get_exclusion_counts(player_ids, guild_id)

        cog = _make_draft_cog(player_repository)
        interaction = _FakeInteraction(guild_id)

        result = await cog._execute_draft(interaction, guild_id, lobby)

        assert result is True
        state = cog.draft_state_manager.get_state(guild_id)
        assert state is not None
        assert set(state.full_exclusion_increment_ids) == set(state.excluded_player_ids)
        assert state.half_exclusion_increment_ids == []
        assert set(state.player_pool_ids).isdisjoint(conditional_ids)
        assert player_repository.get_exclusion_counts(player_ids, guild_id) == exclusion_before

    async def test_captain_eligibility_flags_do_not_limit_selection(self, player_repository):
        """Legacy eligibility values do not affect automatic captains."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 16)
        player_repository.set_captain_eligible(player_ids[0], guild_id, True)

        cog = _make_draft_cog(player_repository)
        interaction = _FakeInteraction(guild_id)

        result = await cog._execute_draft(interaction, guild_id, _make_lobby(player_ids))

        assert result is True
        state = cog.draft_state_manager.get_state(guild_id)
        assert state is not None
        assert player_ids[0] not in {state.captain1_id, state.captain2_id}
        assert {state.captain1_id, state.captain2_id} == set(player_ids[-2:])

    async def test_fails_when_draft_already_active(self, player_repository):
        """An existing active draft blocks a new one and is left untouched."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        for pid in player_ids:
            player_repository.set_captain_eligible(pid, guild_id, True)

        cog = _make_draft_cog(player_repository)
        existing = cog.draft_state_manager.create_draft(guild_id)
        existing.phase = DraftPhase.DRAFTING

        interaction = _FakeInteraction(guild_id)
        result = await cog._execute_draft(interaction, guild_id, _make_lobby(player_ids))

        assert result is False
        # the in-progress draft is preserved, not clobbered
        assert cog.draft_state_manager.get_state(guild_id) is existing
        assert existing.phase == DraftPhase.DRAFTING
        # the progress message was cleaned up and an error was sent
        assert interaction.followup.messages[0].deleted is True
        assert interaction.followup.messages[-1].content.startswith("❌")

    async def test_clears_state_when_draft_embed_fails(self, player_repository):
        """A Discord failure mid-setup must not leave a zombie draft state.

        The draft state is created before the embed is posted; if posting
        fails, the post-creation handler must clear that state so the next
        /shuffle or /draft start is not blocked.
        """
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        for pid in player_ids:
            player_repository.set_captain_eligible(pid, guild_id, True)

        cog = _make_draft_cog(player_repository)
        # the draft embed is posted by editing the progress message — make
        # that edit fail to simulate a Discord API error mid-setup
        interaction = _FakeInteraction(guild_id, fail_edit=True)

        with pytest.raises(RuntimeError):
            await cog._execute_draft(interaction, guild_id, _make_lobby(player_ids))

        # the failure handler cleared the state — no zombie draft left behind
        assert cog.draft_state_manager.get_state(guild_id) is None
        # and the progress message was cleaned up
        assert interaction.followup.messages[0].deleted is True

def _make_final_pick_scenario(
    player_repository, guild_id, player_ids, *, bot, match_service, lobby_manager=None
):
    """Build a cog + drafting state one pick away from completion.

    Shared by the final-pick tests so the ~15 hand-set DraftState fields stay
    in lockstep; only the bot wiring differs between them.
    """
    captain1, captain2 = player_ids[0], player_ids[1]
    cog = DraftCommands(
        bot=bot,
        player_repo=player_repository,
        lobby_manager=lobby_manager if lobby_manager is not None else MagicMock(),
        draft_state_manager=DraftStateManager(),
        draft_service=DraftService(),
        match_service=match_service,
    )
    state = cog.draft_state_manager.create_draft(guild_id)
    state.phase = DraftPhase.DRAFTING
    state.player_pool_ids = player_ids
    state.captain1_id = captain1
    state.captain2_id = captain2
    state.captain1_rating = 1700
    state.captain2_rating = 1600
    state.radiant_captain_id = captain1
    state.dire_captain_id = captain2
    state.current_round_first_captain_id = captain2
    state.radiant_hero_pick_order = 1
    state.dire_hero_pick_order = 2
    state.draft_channel_id = 555_000
    state.draft_message_id = 9_000_001
    state.current_pick_index = 7
    state.radiant_player_ids = [captain1, player_ids[2], player_ids[5], player_ids[6]]
    state.dire_player_ids = [
        captain2,
        player_ids[3],
        player_ids[4],
        player_ids[7],
        player_ids[8],
    ]
    return cog, state


class TestHandlePlayerPick:
    """Regression tests for Discord component handling during active drafting."""

    async def test_pending_match_preserves_conditional_exclusions(self, player_repository):
        """Excluded conditionals retain their half-credit pending-match classification."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 12)
        match_service = _FakeDraftMatchService()
        bot = SimpleNamespace(get_cog=lambda _name: None)
        cog, state = _make_final_pick_scenario(
            player_repository,
            guild_id,
            player_ids[:10],
            bot=bot,
            match_service=match_service,
        )
        state.radiant_player_ids.append(player_ids[9])
        state.excluded_player_ids = [player_ids[10]]
        state.full_exclusion_increment_ids = [player_ids[10]]
        state.half_exclusion_increment_ids = [player_ids[11]]

        pending_match_id = await cog._create_pending_match(guild_id, state)

        assert pending_match_id == 1234
        assert match_service.state.excluded_player_ids == [player_ids[10]]
        assert match_service.state.excluded_conditional_player_ids == [player_ids[11]]
        assert match_service.state.half_exclusion_increment_ids == [player_ids[11]]

    async def test_final_pick_defers_then_edits_original_message(self, player_repository):
        """The last pick does slow match setup, so it must acknowledge the button first."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        captain1 = player_ids[0]
        final_pick = player_ids[9]

        match_service = _FakeDraftMatchService()
        lobby_manager = MagicMock()
        bot = SimpleNamespace(
            betting_service=None,
            lobby_service=None,
            get_cog=lambda _name: None,
        )
        cog, state = _make_final_pick_scenario(
            player_repository, guild_id, player_ids,
            bot=bot, match_service=match_service, lobby_manager=lobby_manager,
        )

        interaction = _FakeComponentInteraction(guild_id, user_id=captain1)

        await cog.handle_player_pick(interaction, guild_id, final_pick)

        assert interaction.response.deferred is True
        assert interaction.response.edit_message_calls == []
        assert interaction.edited_original is True
        assert interaction.message.view is None
        assert state.phase == DraftPhase.COMPLETE
        assert final_pick in state.radiant_player_ids
        assert cog.draft_state_manager.get_state(guild_id) is None
        assert match_service.state.pending_match_id == 1234
        assert match_service.message_info["message_id"] == interaction.message.id
        assert match_service.message_info["channel_id"] == interaction.channel.id
        lobby_manager.reset_lobby.assert_called_once_with(guild_id)

    async def test_final_pick_stores_thread_info_for_record_finalize(self, player_repository):
        """Draft completion must carry the lobby thread id in the pending
        match state, exactly as the shuffle path does.

        Regression: /record finalizes the lobby thread via
        pending_state.thread_shuffle_thread_id (record_match clears the state,
        so there is no fallback). The draft path posted to the thread but never
        stored its id, so drafted matches left their thread stuck on
        "🔒 Draft Complete - Awaiting Results" forever after recording. The
        embed's message id must be stored too so betting updates can refresh
        the wager field inside the thread.
        """
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        captain1 = player_ids[0]
        final_pick = player_ids[9]
        thread_id = 777_001

        class _FakeThread:
            def __init__(self, tid):
                self.id = tid
                self.edits = []
                self.sent = []

            async def edit(self, **kwargs):
                self.edits.append(kwargs)

            async def send(self, content=None, **kwargs):
                msg = SimpleNamespace(
                    id=8_000_000 + len(self.sent),
                    content=content,
                    embed=kwargs.get("embed"),
                )
                self.sent.append(msg)
                return msg

        thread = _FakeThread(thread_id)
        match_service = _FakeDraftMatchService()
        bot = SimpleNamespace(
            betting_service=None,
            lobby_service=SimpleNamespace(
                get_lobby_thread_id=lambda guild_id=None: thread_id
            ),
            get_cog=lambda _name: None,
            get_channel=lambda cid: thread if cid == thread_id else None,
        )
        cog, _ = _make_final_pick_scenario(
            player_repository, guild_id, player_ids,
            bot=bot, match_service=match_service,
        )

        interaction = _FakeComponentInteraction(guild_id, user_id=captain1)

        await cog.handle_player_pick(interaction, guild_id, final_pick)

        # The draft-complete embed was posted to the thread, which was renamed and locked
        assert thread.sent and thread.sent[0].embed is not None
        assert any("Draft Complete" in edit.get("name", "") for edit in thread.edits)
        assert any(edit.get("locked") for edit in thread.edits)
        # The thread id must survive in the pending state so /record can
        # rename + archive the thread once the match result is in.
        assert match_service.state.thread_shuffle_thread_id == thread_id
        # The embed's message id is stored so betting updates refresh the thread copy
        assert match_service.state.thread_shuffle_message_id == thread.sent[0].id

    async def test_pick_rejected_when_turn_passes_during_defer(self, player_repository):
        """A pick that passed the pre-defer turn check is dropped if the turn
        advances while safe_defer is awaited.

        Regression for finding 1: handle_player_pick validates current_captain_id,
        then awaits safe_defer (a network round-trip that yields the event loop),
        then mutates state. discord.py runs each button callback as its own task,
        so a double-click lets a second callback pass the turn check before the
        first finishes. This test reproduces that window directly: the captain's
        turn is advanced *during* safe_defer (as a concurrent pick would do), so
        without the post-defer picker_id guard the stale pick would land on a team
        it no longer controls. With the guard it is rejected.
        """
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        captain1, captain2 = player_ids[0], player_ids[1]
        stale_target = player_ids[3]

        bot = SimpleNamespace(betting_service=None, lobby_service=None, get_cog=lambda _n: None)
        cog = DraftCommands(
            bot=bot,
            player_repo=player_repository,
            lobby_manager=MagicMock(),
            draft_state_manager=DraftStateManager(),
            draft_service=DraftService(),
            match_service=_FakeDraftMatchService(),
        )

        state = cog.draft_state_manager.create_draft(guild_id)
        state.phase = DraftPhase.DRAFTING
        state.player_pool_ids = player_ids
        state.captain1_id = captain1
        state.captain2_id = captain2
        state.radiant_captain_id = captain1
        state.dire_captain_id = captain2
        state.current_round_first_captain_id = captain1
        state.radiant_hero_pick_order = 1
        state.dire_hero_pick_order = 2
        state.draft_channel_id = 555_000
        state.draft_message_id = 9_000_001
        state.radiant_player_ids = [captain1]
        state.dire_player_ids = [captain2]
        assert state.current_captain_id == captain1

        interaction = _FakeComponentInteraction(guild_id, user_id=captain1)

        # Simulate the concurrent winning pick landing DURING the defer round-trip
        # (the event-loop yield where a second discord.py task would mutate state).
        # Drive it through the interaction's own defer() — which the real
        # safe_defer calls — rather than monkeypatching the module-global
        # safe_defer; that keeps the advance on the exact post-defer window and
        # avoids a global-patch race that made this test flake under load.
        # captain1 is on the clock, so this advances the turn to captain2.
        original_defer = interaction.response.defer

        async def _defer_then_advance(**kwargs):
            await original_defer(**kwargs)
            assert state.pick_player(player_ids[2]) is True  # the "other" click

        interaction.response.defer = _defer_then_advance

        await cog.handle_player_pick(interaction, guild_id, stale_target)

        # The stale pick must NOT have landed: it's captain2's turn now, and
        # captain1's button can't drop a player onto captain2's clock.
        assert stale_target not in state.radiant_player_ids
        assert stale_target not in state.dire_player_ids
        assert state.current_pick_index == 1  # only the concurrent pick advanced it
        assert state.current_captain_id == captain2
        # The rejected click was told the pick failed instead of corrupting state.
        assert len(interaction.followup.messages) == 1
        assert interaction.followup.messages[0].content.startswith("❌")


class _FakeWinnerChoiceResponse:
    """Records edit_message / send_message for the pre-draft choice handlers."""

    def __init__(self):
        self.edit_message_calls = []
        self.sent_messages = []

    async def edit_message(self, **kwargs):
        self.edit_message_calls.append(kwargs)

    async def send_message(self, content=None, **kwargs):
        self.sent_messages.append({"content": content, **kwargs})


class _FakePingChannel:
    """Channel whose fetch_message yields control, mimicking a network round-trip."""

    def __init__(self):
        self.id = 555_000

    async def fetch_message(self, message_id):
        await asyncio.sleep(0)
        return SimpleNamespace(delete=AsyncMock())


class _FakeWinnerChoiceInteraction:
    def __init__(self, guild_id, user_id):
        self.guild = _FakeGuild(guild_id)
        self.guild_id = guild_id
        self.user = SimpleNamespace(id=user_id)
        self.channel = _FakePingChannel()
        self.response = _FakeWinnerChoiceResponse()


class TestPreDraftChoiceDoubleClick:
    """Regression for finding 18: a double-clicked pre-draft choice is rejected."""

    def _make_winner_choice_state(self, cog, guild_id, winner_id):
        state = cog.draft_state_manager.create_draft(guild_id)
        state.phase = DraftPhase.WINNER_CHOICE
        state.player_pool_ids = [winner_id, winner_id + 1] + list(range(1, 9))
        state.captain1_id = winner_id
        state.captain2_id = winner_id + 1
        state.coinflip_winner_id = winner_id
        # A ping message id makes _delete_captain_ping_message await a fetch (the yield).
        state.captain_ping_message_id = 777_000
        return state

    async def test_double_click_winner_chose_side_only_advances_once(self, player_repository):
        """Two concurrent 'choose side' clicks must advance the phase exactly once.

        handle_winner_chose_side awaits _delete_captain_ping_message (a network
        round-trip) between reading the phase and mutating it. A double-click
        lets both callbacks pass interaction_check and read phase WINNER_CHOICE;
        without a post-yield re-check both would mutate state. The second click
        must instead be rejected with an ephemeral 'already made' message.
        """
        guild_id = TEST_GUILD_ID
        winner_id = 50_001  # registered by the fixture below
        _register_draft_players(player_repository, guild_id, 10)
        cog = _make_draft_cog(player_repository)
        state = self._make_winner_choice_state(cog, guild_id, winner_id)

        inter_a = _FakeWinnerChoiceInteraction(guild_id, user_id=winner_id)
        inter_b = _FakeWinnerChoiceInteraction(guild_id, user_id=winner_id)

        await asyncio.gather(
            cog.handle_winner_chose_side(inter_a, guild_id),
            cog.handle_winner_chose_side(inter_b, guild_id),
        )

        # The phase advanced exactly once...
        assert state.phase == DraftPhase.WINNER_SIDE_CHOICE
        assert state.winner_choice_type == "side"
        # ...one click edited the message to show the side choice...
        total_edits = len(inter_a.response.edit_message_calls) + len(
            inter_b.response.edit_message_calls
        )
        assert total_edits == 1
        # ...and the losing click was told the choice was already made.
        rejections = inter_a.response.sent_messages + inter_b.response.sent_messages
        assert len(rejections) == 1
        assert "already been made" in rejections[0]["content"]

    async def test_double_click_final_choice_only_starts_player_draft_once(
        self, player_repository, monkeypatch
    ):
        """Two final pre-draft clicks cannot both enter the player-draft start path."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        winner_id, loser_id = player_ids[:2]
        cog = _make_draft_cog(player_repository)
        state = cog.draft_state_manager.create_draft(guild_id)
        state.phase = DraftPhase.LOSER_CHOICE
        state.player_pool_ids = player_ids
        state.player_pool_data = {
            player_id: {"rating": 1400.0 + index * 30}
            for index, player_id in enumerate(player_ids)
        }
        state.captain1_id = winner_id
        state.captain2_id = loser_id
        state.coinflip_winner_id = winner_id
        state.winner_choice_type = "side"
        state.winner_choice_value = "radiant"
        state.radiant_captain_id = winner_id
        state.dire_captain_id = loser_id

        async def yielding_name_lookup(_guild, _user_id):
            await asyncio.sleep(0)
            return "Captain"

        show_draft_ui = AsyncMock()
        monkeypatch.setattr(cog, "_get_member_name", yielding_name_lookup)
        monkeypatch.setattr(cog, "_show_draft_ui", show_draft_ui)
        monkeypatch.setattr("commands.draft.get_neon_service", lambda _bot: None)

        inter_a = _FakeWinnerChoiceInteraction(guild_id, user_id=loser_id)
        inter_b = _FakeWinnerChoiceInteraction(guild_id, user_id=loser_id)

        await asyncio.gather(
            cog.handle_hero_pick_choice(inter_a, guild_id, "first", is_winner=False),
            cog.handle_hero_pick_choice(inter_b, guild_id, "first", is_winner=False),
        )

        assert state.phase == DraftPhase.DRAFTING
        show_draft_ui.assert_awaited_once()
        rejections = inter_a.response.sent_messages + inter_b.response.sent_messages
        assert len(rejections) == 1
        assert "already been made" in rejections[0]["content"]


class TestShuffleDraftRedirect:
    """/shuffle hands lobbies of >=15 players to Immortal Draft."""

    async def test_redirects_without_adding_a_duplicate_message(self, monkeypatch):
        """A >=15-player /shuffle runs _execute_draft and adds no message of
        its own — even on failure, since _execute_draft owns its messaging."""
        guild_id = TEST_GUILD_ID
        draft_cog = MagicMock()
        # the draft "fails" (returns False) but messaged the user itself
        draft_cog._execute_draft = AsyncMock(return_value=False)
        bot = MagicMock()
        bot.get_cog = MagicMock(return_value=draft_cog)

        match_cog = MatchCommands(bot, MagicMock(), MagicMock(), MagicMock())

        lobby = _make_lobby(list(range(60001, 60016)))  # 15 regular players
        monkeypatch.setattr(
            match_cog,
            "_validate_shuffle_preconditions",
            AsyncMock(return_value=lobby),
        )

        interaction = _FakeInteraction(guild_id)
        await match_cog._execute_shuffle(interaction, interaction.guild, guild_id, None)

        # the draft was started for the oversized lobby...
        draft_cog._execute_draft.assert_awaited_once()
        # ...and match.py posted nothing of its own (the old code added a
        # misleading "check captain eligibility" message here)
        assert interaction.followup.messages == []


class TestDraftingViewInteractionCheck:
    """Regression tests for DraftingView.interaction_check (finding 4).

    Non-participants clicking pick or side-preference buttons must be rejected
    without hitting any downstream handler.
    """

    def _make_view(self, captain1=1, captain2=2, pool=(1, 2, 3, 4, 5, 6, 7, 8)):
        """Build a minimal DraftingView with no real DraftCommands cog."""
        from commands.draft import DraftingView

        cog = MagicMock()
        return DraftingView(
            cog=cog,
            guild_id=99,
            available_players=[],
            current_captain_id=captain1,
            captain_ids={captain1, captain2},
            player_pool_ids=list(pool),
        )

    @pytest.mark.asyncio
    async def test_participant_allowed(self):
        """A player in the pool passes interaction_check."""
        view = self._make_view()
        interaction = MagicMock()
        interaction.user.id = 5  # in pool
        interaction.response = AsyncMock()

        result = await view.interaction_check(interaction)

        assert result is True
        interaction.response.send_message.assert_not_called()

    @pytest.mark.asyncio
    async def test_captain_allowed(self):
        """A captain passes interaction_check."""
        view = self._make_view()
        interaction = MagicMock()
        interaction.user.id = 1  # captain1
        interaction.response = AsyncMock()

        result = await view.interaction_check(interaction)

        assert result is True

    @pytest.mark.asyncio
    async def test_non_participant_rejected(self):
        """A user outside the draft is rejected and gets an ephemeral error."""
        view = self._make_view()
        interaction = MagicMock()
        interaction.user.id = 999  # not in pool or captains
        interaction.response = AsyncMock()

        result = await view.interaction_check(interaction)

        assert result is False
        interaction.response.send_message.assert_awaited_once()
        call_kwargs = interaction.response.send_message.call_args
        assert call_kwargs.kwargs.get("ephemeral") is True

    @pytest.mark.asyncio
    async def test_empty_participant_ids_allows_all(self):
        """When no participant IDs are provided (preview mode), everyone passes."""
        from commands.draft import DraftingView

        cog = MagicMock()
        view = DraftingView(
            cog=cog,
            guild_id=99,
            available_players=[],
            current_captain_id=None,
            # no captain_ids / player_pool_ids → preview mode
        )
        interaction = MagicMock()
        interaction.user.id = 42
        interaction.response = AsyncMock()

        result = await view.interaction_check(interaction)

        assert result is True


class TestDraftTimeoutOwnership:
    """A stale or superseded view's timeout must not clear a newer draft.

    Regression tests for the bug where every pick built a fresh DraftingView
    but never stopped the superseded one, so a stale view's 600s timeout
    could wipe whatever draft state the guild had (e.g. after /draft restart)
    and overwrite the new draft's message with "Draft Timed Out".
    """

    def _make_cog(self):
        return DraftCommands(
            bot=MagicMock(),
            player_repo=MagicMock(),
            lobby_manager=MagicMock(),
            draft_state_manager=DraftStateManager(),
            draft_service=DraftService(),
            match_service=None,
        )

    def _make_view(self, cog, guild_id, state):
        from commands.draft import DraftingView

        return DraftingView(
            cog=cog,
            guild_id=guild_id,
            available_players=[],
            current_captain_id=1,
            draft_state=state,
        )

    @pytest.mark.asyncio
    async def test_stale_view_timeout_does_not_clear_restarted_draft(self):
        """A view left over from a restarted draft must not wipe the new one."""
        cog = self._make_cog()
        guild_id = 123
        old_state = cog.draft_state_manager.create_draft(guild_id)
        stale_view = self._make_view(cog, guild_id, old_state)
        cog._track_draft_view(guild_id, stale_view)

        # /draft restart: clear state, stop the tracked view, start a new draft
        cog.draft_state_manager.clear_state(guild_id)
        cog._stop_tracked_draft_view(guild_id)
        new_state = cog.draft_state_manager.create_draft(guild_id)

        await cog._handle_draft_timeout(guild_id, view=stale_view)

        assert cog.draft_state_manager.get_state(guild_id) is new_state

    @pytest.mark.asyncio
    async def test_superseded_view_timeout_does_not_clear_state(self):
        """A view replaced by a newer pick UI is stopped and cannot time out the draft."""
        cog = self._make_cog()
        guild_id = 123
        state = cog.draft_state_manager.create_draft(guild_id)
        old_view = self._make_view(cog, guild_id, state)
        cog._track_draft_view(guild_id, old_view)
        new_view = self._make_view(cog, guild_id, state)
        cog._track_draft_view(guild_id, new_view)

        # Tracking the replacement stopped the superseded view outright.
        assert old_view.is_finished()

        await cog._handle_draft_timeout(guild_id, view=old_view)

        assert cog.draft_state_manager.get_state(guild_id) is state

    @pytest.mark.asyncio
    async def test_current_view_timeout_still_clears_state(self):
        """The live view's timeout must keep working."""
        cog = self._make_cog()
        guild_id = 123
        state = cog.draft_state_manager.create_draft(guild_id)
        view = self._make_view(cog, guild_id, state)
        cog._track_draft_view(guild_id, view)

        await cog._handle_draft_timeout(guild_id, view=view)

        assert cog.draft_state_manager.get_state(guild_id) is None

    @pytest.mark.asyncio
    async def test_sample_view_timeout_does_not_clear_real_draft(self):
        """A sample UI view (mock state, untracked) must not clear a real draft."""
        cog = self._make_cog()
        guild_id = 123
        real_state = cog.draft_state_manager.create_draft(guild_id)
        mock_state = DraftState(guild_id=guild_id)
        sample_view = self._make_view(cog, guild_id, mock_state)

        await cog._handle_draft_timeout(guild_id, view=sample_view)

        assert cog.draft_state_manager.get_state(guild_id) is real_state
