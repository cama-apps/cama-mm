"""
Tests for Immortal Draft functionality.
"""

import asyncio
import logging
from datetime import datetime
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.draft import DraftCommands
from commands.match import MatchCommands
from domain.models.draft import SNAKE_DRAFT_ORDER, DraftPhase, DraftState
from domain.models.lobby import Lobby
from domain.services.draft_service import DraftService
from repositories.player_repository import PlayerRepository
from services.draft_state_manager import DraftStateManager
from tests.conftest import TEST_GUILD_ID


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
        state.player_draft_first_captain_id = 100  # Radiant picks first

        # Pick 0: first captain (Radiant)
        state.current_pick_index = 0
        assert state.current_captain_id == 100

        # Pick 1: second captain (Dire) - snake draft
        state.current_pick_index = 1
        assert state.current_captain_id == 200

        # Pick 2: second captain (Dire) - still Dire's turn
        state.current_pick_index = 2
        assert state.current_captain_id == 200

        # Pick 3: first captain (Radiant)
        state.current_pick_index = 3
        assert state.current_captain_id == 100

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
        state.player_draft_first_captain_id = 100

        # Pick 0: Radiant has 1 pick
        state.current_pick_index = 0
        assert state.picks_remaining_this_turn == 1

        # Pick 1: Dire has 2 picks
        state.current_pick_index = 1
        assert state.picks_remaining_this_turn == 2

        # Pick 3: Radiant has 2 picks
        state.current_pick_index = 3
        assert state.picks_remaining_this_turn == 2

    def test_lower_rated_captain_id(self):
        """Lower rated captain is correctly identified."""
        state = DraftState(guild_id=123)
        state.captain1_id = 100
        state.captain2_id = 200
        state.captain1_rating = 1500.0
        state.captain2_rating = 1600.0

        assert state.lower_rated_captain_id == 100

        # Reverse ratings
        state.captain1_rating = 1700.0
        assert state.lower_rated_captain_id == 200

    def test_pick_player_success(self):
        """Picking a player adds them to correct team."""
        state = DraftState(guild_id=123)
        state.phase = DraftPhase.DRAFTING
        state.player_pool_ids = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        state.radiant_captain_id = 100
        state.dire_captain_id = 200
        state.player_draft_first_captain_id = 100

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
        state.player_draft_first_captain_id = 100

        result = state.pick_player(999)  # Not in pool
        assert result is False

    def test_pick_player_rejects_wrong_picker(self):
        """pick_player(picker_id=...) refuses a captain who isn't on the clock.

        This is the synchronous turn guard that closes the concurrent-pick race
        (finding 1): two button callbacks from the same captain both pass the
        async pre-defer turn check, then both reach pick_player. The first
        advances the snake order; the second must be rejected here because it is
        no longer that captain's turn.
        """
        state = DraftState(guild_id=123)
        state.phase = DraftPhase.DRAFTING
        state.player_pool_ids = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        state.radiant_captain_id = 100
        state.dire_captain_id = 200
        state.player_draft_first_captain_id = 100  # Radiant (100) picks at index 0

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
            eligible_ids=[100, 200, 300],
            player_ratings=ratings,
            specified_captain1=100,
            specified_captain2=200,
        )

        assert result.captain1_id == 100
        assert result.captain2_id == 200
        assert result.captain1_rating == 1500.0
        assert result.captain2_rating == 1600.0

    def test_select_captains_not_enough_eligible(self):
        """Raises error when not enough eligible captains."""
        service = DraftService()
        ratings = {100: 1500.0}

        with pytest.raises(ValueError, match="at least 2"):
            service.select_captains(
                eligible_ids=[100],
                player_ratings=ratings,
            )

    def test_select_captains_random_selection(self):
        """When neither specified, randomly selects both."""
        service = DraftService()
        ratings = {100: 1500.0, 200: 1500.0, 300: 1500.0}

        result = service.select_captains(
            eligible_ids=[100, 200, 300],
            player_ratings=ratings,
        )

        assert result.captain1_id in [100, 200, 300]
        assert result.captain2_id in [100, 200, 300]
        assert result.captain1_id != result.captain2_id

    def test_select_captains_weighted_random_prefers_similar(self):
        """Weighted random prefers captains with similar ratings."""
        service = DraftService(rating_weight_factor=100.0)
        # Captain 100 at 1500, captain 200 at 1500, captain 300 at 2000
        ratings = {100: 1500.0, 200: 1500.0, 300: 2000.0}

        # Run many times to check statistical preference
        close_count = 0
        far_count = 0
        for _ in range(100):
            result = service.select_captains(
                eligible_ids=[100, 200, 300],
                player_ratings=ratings,
                specified_captain1=100,  # Force captain1 to be 100
            )
            if result.captain2_id == 200:  # Same rating
                close_count += 1
            else:
                far_count += 1

        # Should strongly prefer the closer-rated captain
        assert close_count > far_count

    def test_select_player_pool_exact_size(self):
        """When lobby equals pool size, all selected."""
        service = DraftService()

        result = service.select_player_pool(
            lobby_player_ids=[1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            exclusion_counts={},
            pool_size=10,
        )

        assert len(result.selected_ids) == 10
        assert result.excluded_ids == []

    def test_select_player_pool_with_exclusions(self):
        """Players with higher exclusion counts are prioritized."""
        service = DraftService()

        result = service.select_player_pool(
            lobby_player_ids=[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            exclusion_counts={11: 5, 12: 3},  # 11 and 12 excluded most
            pool_size=10,
        )

        # 11 and 12 should be included due to high exclusion counts
        assert 11 in result.selected_ids
        assert 12 in result.selected_ids
        assert len(result.excluded_ids) == 2

    def test_select_player_pool_forced_include(self):
        """Forced players are always included."""
        service = DraftService()

        result = service.select_player_pool(
            lobby_player_ids=[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            exclusion_counts={},
            forced_include_ids=[11, 12],  # Force these captains
            pool_size=10,
        )

        assert 11 in result.selected_ids
        assert 12 in result.selected_ids

    def test_select_player_pool_not_enough(self):
        """Raises error when lobby smaller than pool size."""
        service = DraftService()

        with pytest.raises(ValueError, match="Need at least"):
            service.select_player_pool(
                lobby_player_ids=[1, 2, 3],
                exclusion_counts={},
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

    def test_determine_lower_rated_captain(self):
        """Correctly identifies lower-rated captain."""
        service = DraftService()

        result = service.determine_lower_rated_captain(
            captain1_id=100,
            captain1_rating=1500.0,
            captain2_id=200,
            captain2_rating=1600.0,
        )
        assert result == 100

        result = service.determine_lower_rated_captain(
            captain1_id=100,
            captain1_rating=1700.0,
            captain2_id=200,
            captain2_rating=1600.0,
        )
        assert result == 200


class TestSnakeDraftOrder:
    """Tests for snake draft order constant."""

    def test_snake_draft_order_length(self):
        """Snake draft order has 8 picks."""
        assert len(SNAKE_DRAFT_ORDER) == 8

    def test_snake_draft_order_pattern(self):
        """Snake draft follows 1-2-2-2-1 pattern."""
        # [0, 1, 1, 0, 0, 1, 1, 0] means:
        # Pick 1: Captain 0
        # Pick 2-3: Captain 1
        # Pick 4-5: Captain 0
        # Pick 6-7: Captain 1
        # Pick 8: Captain 0
        assert SNAKE_DRAFT_ORDER[0] == 0  # First captain
        assert SNAKE_DRAFT_ORDER[1] == 1  # Second captain
        assert SNAKE_DRAFT_ORDER[2] == 1  # Second captain
        assert SNAKE_DRAFT_ORDER[3] == 0  # First captain
        assert SNAKE_DRAFT_ORDER[4] == 0  # First captain
        assert SNAKE_DRAFT_ORDER[5] == 1  # Second captain
        assert SNAKE_DRAFT_ORDER[6] == 1  # Second captain
        assert SNAKE_DRAFT_ORDER[7] == 0  # First captain


class TestSnakeDraftPickOrder:
    """Integration test: drive a complete draft and assert pick sequence and guild isolation."""

    def _make_state(self, guild_id: int) -> DraftState:
        """Return a DRAFTING-phase state with two captains and 8 draftable players."""
        state = DraftState(guild_id=guild_id)
        state.radiant_captain_id = 1
        state.dire_captain_id = 2
        state.captain1_id = 1
        state.captain2_id = 2
        # player_draft_first_captain_id = 1 means captain 1 (index 0) picks first
        state.player_draft_first_captain_id = 1
        state.player_pool_ids = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]  # includes captains
        state.phase = DraftPhase.DRAFTING
        return state

    def test_full_snake_draft_assigns_correct_sides(self):
        """Complete 8-pick snake draft: verify pick order matches SNAKE_DRAFT_ORDER
        and both teams end up with 4 players each plus their captain.

        SNAKE_DRAFT_ORDER = [0,1,1,0,0,1,1,0]  (0=captain1/Radiant, 1=captain2/Dire)
        Expected radiant picks: indices 0,3,4,7 → players 11,14,15,18 (sequential)
        Expected dire picks:    indices 1,2,5,6 → players 12,13,16,17
        """
        state = self._make_state(guild_id=1001)
        # Draftable players: 11-18 (captains 1 and 2 are pre-assigned, not in pool)
        state.player_pool_ids = [11, 12, 13, 14, 15, 16, 17, 18]

        expected_radiant_picks = []
        expected_dire_picks = []
        pick_log = []

        for pick_num, picker_index in enumerate(SNAKE_DRAFT_ORDER):
            expected_captain = state.current_captain_id
            # picker_index 0 → captain1 (radiant), 1 → captain2 (dire)
            if picker_index == 0:
                assert expected_captain == 1, f"Pick {pick_num}: expected captain1"
            else:
                assert expected_captain == 2, f"Pick {pick_num}: expected captain2"

            player_to_pick = state.available_player_ids[0]
            success = state.pick_player(player_to_pick)
            assert success, f"pick_player failed at pick {pick_num}"
            pick_log.append((picker_index, player_to_pick))

            if picker_index == 0:
                expected_radiant_picks.append(player_to_pick)
            else:
                expected_dire_picks.append(player_to_pick)

        assert state.phase == DraftPhase.COMPLETE
        assert state.current_pick_index == 8
        assert set(state.radiant_player_ids) == set(expected_radiant_picks)
        assert set(state.dire_player_ids) == set(expected_dire_picks)
        assert len(state.radiant_player_ids) == 4
        assert len(state.dire_player_ids) == 4
        assert not state.available_player_ids

    def test_guild_isolation_draft_states(self):
        """Two guilds can run independent drafts; picks in one do not affect the other."""
        state_a = self._make_state(guild_id=2001)
        state_b = self._make_state(guild_id=2002)
        state_a.player_pool_ids = [11, 12, 13, 14, 15, 16, 17, 18]
        state_b.player_pool_ids = [21, 22, 23, 24, 25, 26, 27, 28]

        # Pick all 8 players in guild A
        for _ in SNAKE_DRAFT_ORDER:
            player = state_a.available_player_ids[0]
            state_a.pick_player(player)

        # Guild B should still be at pick 0 with all 8 players available
        assert state_b.current_pick_index == 0
        assert len(state_b.available_player_ids) == 8
        assert state_b.phase == DraftPhase.DRAFTING

        # Pick all 8 players in guild B
        for _ in SNAKE_DRAFT_ORDER:
            player = state_b.available_player_ids[0]
            state_b.pick_player(player)

        assert state_a.phase == DraftPhase.COMPLETE
        assert state_b.phase == DraftPhase.COMPLETE
        # No overlap between teams across guilds
        assert not (set(state_a.radiant_player_ids) & set(state_b.radiant_player_ids))


class TestForceRandomCaptains:
    """Tests for force_random_captains functionality used in shuffle auto-redirect.

    Note: force_random_captains skips the 60s wait but still respects captain eligibility.
    Only players who did /setcaptain yes can be selected as captain.
    """

    def test_select_captains_force_random_all_eligible(self):
        """DraftService.select_captains picks from the eligible_ids list provided."""
        service = DraftService()
        # All players have the same rating - simulates random selection
        ratings = {1: 1500.0, 2: 1500.0, 3: 1500.0, 4: 1500.0, 5: 1500.0}

        # With force_random_captains, we pass all players as eligible
        # and let select_captains randomly pick two with specified captains
        selected = [1, 2]  # Simulating random.sample result

        result = service.select_captains(
            eligible_ids=[1, 2, 3, 4, 5],
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
                eligible_ids=player_ids,
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

    def test_lobby_player_count_includes_conditional(self):
        """Verify total count includes both regular and conditional players.

        Note: Immortal Draft triggers on regular_count >= 15, not total_count.
        This test verifies the count methods work correctly.
        """
        from datetime import datetime

        from domain.models.lobby import Lobby

        lobby = Lobby(lobby_id=1, created_by=999, created_at=datetime.now())
        # Add 10 regular players
        for i in range(1, 11):
            lobby.add_player(i)
        # Add 6 conditional players
        for i in range(11, 17):
            lobby.add_conditional_player(i)

        total = lobby.get_total_count()
        regular = lobby.get_player_count()
        assert total == 16
        assert regular == 10
        # Immortal Draft requires 15+ regular players, so this would NOT trigger it
        assert regular < 15


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


class TestConditionalPlayerPromotion:
    """
    Tests for conditional player promotion logic in draft.

    Conditional players ("froglings") are only promoted to the draft pool
    when there aren't enough regular players. Regular players are always
    included first; conditional players are randomly selected to fill
    remaining spots up to 10.
    """

    def test_enough_regular_players_excludes_conditional(self):
        """When >= 10 regular players, conditional players are excluded."""
        from datetime import datetime

        from domain.models.lobby import Lobby

        lobby = Lobby(lobby_id=1, created_by=999, created_at=datetime.now())
        # Add 12 regular players
        for i in range(1, 13):
            lobby.add_player(i)
        # Add 3 conditional players
        for i in range(101, 104):
            lobby.add_conditional_player(i)

        regular_players = list(lobby.players)
        conditional_players = list(lobby.conditional_players)
        DRAFT_POOL_SIZE = 10

        if len(regular_players) >= DRAFT_POOL_SIZE:
            lobby_player_ids = regular_players
        else:
            needed = DRAFT_POOL_SIZE - len(regular_players)
            import random
            promoted_conditional = random.sample(
                conditional_players, min(needed, len(conditional_players))
            )
            lobby_player_ids = regular_players + promoted_conditional

        # All should be regular players, no conditional
        assert len(lobby_player_ids) == 12
        assert all(pid < 100 for pid in lobby_player_ids)
        assert not any(pid >= 100 for pid in lobby_player_ids)

    def test_promotes_conditional_when_not_enough_regular(self):
        """When < 10 regular players, promotes random conditional players."""
        import random
        from datetime import datetime

        from domain.models.lobby import Lobby

        lobby = Lobby(lobby_id=1, created_by=999, created_at=datetime.now())
        # Add 8 regular players
        for i in range(1, 9):
            lobby.add_player(i)
        # Add 4 conditional players
        for i in range(101, 105):
            lobby.add_conditional_player(i)

        regular_players = list(lobby.players)
        conditional_players = list(lobby.conditional_players)
        DRAFT_POOL_SIZE = 10

        if len(regular_players) >= DRAFT_POOL_SIZE:
            lobby_player_ids = regular_players
        else:
            needed = DRAFT_POOL_SIZE - len(regular_players)
            promoted_conditional = random.sample(
                conditional_players, min(needed, len(conditional_players))
            )
            lobby_player_ids = regular_players + promoted_conditional

        # Should have 10 players: 8 regular + 2 promoted conditional
        assert len(lobby_player_ids) == 10

        # All 8 regular players should be included
        for i in range(1, 9):
            assert i in lobby_player_ids

        # Exactly 2 conditional players should be promoted
        conditional_in_pool = [pid for pid in lobby_player_ids if pid >= 100]
        assert len(conditional_in_pool) == 2

    def test_promotes_all_conditional_if_needed(self):
        """When regular + conditional < 10, promotes all conditional."""
        import random
        from datetime import datetime

        from domain.models.lobby import Lobby

        lobby = Lobby(lobby_id=1, created_by=999, created_at=datetime.now())
        # Add 7 regular players
        for i in range(1, 8):
            lobby.add_player(i)
        # Add 3 conditional players (total 10)
        for i in range(101, 104):
            lobby.add_conditional_player(i)

        regular_players = list(lobby.players)
        conditional_players = list(lobby.conditional_players)
        DRAFT_POOL_SIZE = 10

        if len(regular_players) >= DRAFT_POOL_SIZE:
            lobby_player_ids = regular_players
        else:
            needed = DRAFT_POOL_SIZE - len(regular_players)  # 3 needed
            promoted_conditional = random.sample(
                conditional_players, min(needed, len(conditional_players))
            )
            lobby_player_ids = regular_players + promoted_conditional

        # Should have 10 players: 7 regular + 3 conditional
        assert len(lobby_player_ids) == 10

        # All 7 regular players should be included
        for i in range(1, 8):
            assert i in lobby_player_ids

        # All 3 conditional players should be promoted
        for i in range(101, 104):
            assert i in lobby_player_ids

    def test_conditional_promotion_is_random(self):
        """Conditional player promotion uses random selection, not rating."""
        import random
        from datetime import datetime

        from domain.models.lobby import Lobby

        lobby = Lobby(lobby_id=1, created_by=999, created_at=datetime.now())
        # Add 8 regular players
        for i in range(1, 9):
            lobby.add_player(i)
        # Add 5 conditional players
        for i in range(101, 106):
            lobby.add_conditional_player(i)

        regular_players = list(lobby.players)
        conditional_players = list(lobby.conditional_players)
        DRAFT_POOL_SIZE = 10

        # Run multiple times to verify randomness
        promoted_sets = []
        for _ in range(20):
            needed = DRAFT_POOL_SIZE - len(regular_players)  # 2 needed
            promoted = tuple(sorted(random.sample(
                conditional_players, min(needed, len(conditional_players))
            )))
            promoted_sets.append(promoted)

        # Should have multiple different combinations (randomness)
        unique_combinations = set(promoted_sets)
        # With 5 choose 2 = 10 possible combinations, we should see variety
        assert len(unique_combinations) > 1, "Promotion should be random, not deterministic"

    def test_exactly_ten_regular_no_conditional_needed(self):
        """Exactly 10 regular players means no conditional promotion."""
        import random
        from datetime import datetime

        from domain.models.lobby import Lobby

        lobby = Lobby(lobby_id=1, created_by=999, created_at=datetime.now())
        # Add exactly 10 regular players
        for i in range(1, 11):
            lobby.add_player(i)
        # Add 2 conditional players
        for i in range(101, 103):
            lobby.add_conditional_player(i)

        regular_players = list(lobby.players)
        conditional_players = list(lobby.conditional_players)
        DRAFT_POOL_SIZE = 10

        if len(regular_players) >= DRAFT_POOL_SIZE:
            lobby_player_ids = regular_players
        else:
            needed = DRAFT_POOL_SIZE - len(regular_players)
            promoted_conditional = random.sample(
                conditional_players, min(needed, len(conditional_players))
            )
            lobby_player_ids = regular_players + promoted_conditional

        # Should have exactly 10 regular players
        assert len(lobby_player_ids) == 10
        assert all(pid < 100 for pid in lobby_player_ids)
        # No conditional players promoted
        assert 101 not in lobby_player_ids
        assert 102 not in lobby_player_ids


class TestCandidatePrePruning:
    """Tests for pre-pruning logic when too many candidates for balanced selection."""

    def test_prune_priority_regular_over_conditional(self):
        """Regular players are prioritized over conditional players."""
        from domain.models.player import Player

        # Create mixed players: 10 regular (IDs 1-10), 8 conditional (IDs 101-108)
        regular_ids = set(range(1, 11))
        players = []
        for i in range(1, 11):
            players.append(Player(
                name=f"Regular{i}",
                discord_id=i,
                glicko_rating=1500.0,
            ))
        for i in range(101, 109):
            players.append(Player(
                name=f"Conditional{i}",
                discord_id=i,
                glicko_rating=1600.0,  # Higher rating but conditional
            ))

        exclusion_counts = {p.discord_id: 0 for p in players}
        MAX_CANDIDATES = 14

        def prune_priority(p):
            is_regular = p.discord_id in regular_ids
            exc_count = exclusion_counts.get(p.discord_id, 0)
            rating = p.glicko_rating or 1500.0
            return (0 if is_regular else 1, -exc_count, -rating)

        sorted_candidates = sorted(players, key=prune_priority)
        kept = sorted_candidates[:MAX_CANDIDATES]
        pruned = sorted_candidates[MAX_CANDIDATES:]

        # All 10 regular players should be kept
        kept_ids = {p.discord_id for p in kept}
        for i in range(1, 11):
            assert i in kept_ids, f"Regular player {i} should be kept"

        # 4 conditional players should be kept (10 + 4 = 14)
        # 4 conditional players should be pruned (8 - 4 = 4)
        assert len(pruned) == 4
        for p in pruned:
            assert p.discord_id >= 101, "Only conditional players should be pruned"

    def test_prune_priority_higher_exclusion_first(self):
        """Players with higher exclusion counts are prioritized."""
        from domain.models.player import Player

        regular_ids = set(range(1, 17))  # 16 regular players
        players = [
            Player(name=f"Player{i}", discord_id=i, glicko_rating=1500.0)
            for i in range(1, 17)
        ]

        # Vary exclusion counts: players 1-5 have high counts, 6-16 have low
        exclusion_counts = {}
        for i in range(1, 6):
            exclusion_counts[i] = 10  # High exclusion count
        for i in range(6, 17):
            exclusion_counts[i] = 0  # Low exclusion count

        MAX_CANDIDATES = 14

        def prune_priority(p):
            is_regular = p.discord_id in regular_ids
            exc_count = exclusion_counts.get(p.discord_id, 0)
            rating = p.glicko_rating or 1500.0
            return (0 if is_regular else 1, -exc_count, -rating)

        sorted_candidates = sorted(players, key=prune_priority)
        kept = sorted_candidates[:MAX_CANDIDATES]
        pruned = sorted_candidates[MAX_CANDIDATES:]

        # High-exclusion players (1-5) should all be kept
        kept_ids = {p.discord_id for p in kept}
        for i in range(1, 6):
            assert i in kept_ids, f"High-exclusion player {i} should be kept"

        # 2 low-exclusion players should be pruned (16 - 14 = 2)
        assert len(pruned) == 2
        for p in pruned:
            assert exclusion_counts.get(p.discord_id, 0) == 0, "Low-exclusion players should be pruned"

    def test_prune_priority_higher_rating_tiebreaker(self):
        """Higher rating breaks ties when regular status and exclusion are equal."""
        from domain.models.player import Player

        regular_ids = set(range(1, 17))  # 16 regular players
        players = []
        for i in range(1, 17):
            players.append(Player(
                name=f"Player{i}",
                discord_id=i,
                glicko_rating=1000.0 + i * 100,  # Player 16 has highest rating
            ))

        exclusion_counts = {p.discord_id: 0 for p in players}  # All equal
        MAX_CANDIDATES = 14

        def prune_priority(p):
            is_regular = p.discord_id in regular_ids
            exc_count = exclusion_counts.get(p.discord_id, 0)
            rating = p.glicko_rating or 1500.0
            return (0 if is_regular else 1, -exc_count, -rating)

        sorted_candidates = sorted(players, key=prune_priority)
        pruned = sorted_candidates[MAX_CANDIDATES:]

        # Highest rated players (ID 3-16 with ratings 1300-2600) should be kept
        # Lowest rated players (ID 1-2 with ratings 1100-1200) should be pruned
        pruned_ids = {p.discord_id for p in pruned}
        assert len(pruned) == 2
        assert 1 in pruned_ids, "Lowest rated player should be pruned"
        assert 2 in pruned_ids, "Second lowest rated player should be pruned"

    def test_no_pruning_at_threshold(self):
        """No pruning when candidates <= MAX_CANDIDATES."""
        from domain.models.player import Player

        regular_ids = set(range(1, 15))  # 14 regular players (exactly at limit)
        players = [
            Player(name=f"Player{i}", discord_id=i, glicko_rating=1500.0)
            for i in range(1, 15)
        ]

        exclusion_counts = {p.discord_id: 0 for p in players}
        MAX_CANDIDATES = 14

        # Simulate the condition check
        if len(players) > MAX_CANDIDATES:
            def prune_priority(p):
                is_regular = p.discord_id in regular_ids
                exc_count = exclusion_counts.get(p.discord_id, 0)
                rating = p.glicko_rating or 1500.0
                return (0 if is_regular else 1, -exc_count, -rating)

            sorted_candidates = sorted(players, key=prune_priority)
            candidates_for_pool = sorted_candidates[:MAX_CANDIDATES]
            pre_excluded = sorted_candidates[MAX_CANDIDATES:]
        else:
            candidates_for_pool = players
            pre_excluded = []

        # No pruning should occur
        assert len(candidates_for_pool) == 14
        assert len(pre_excluded) == 0

    def test_pruning_at_threshold_plus_one(self):
        """Pruning occurs when candidates = MAX_CANDIDATES + 1."""
        from domain.models.player import Player

        regular_ids = set(range(1, 16))  # 15 regular players (1 over limit)
        players = [
            Player(name=f"Player{i}", discord_id=i, glicko_rating=1500.0)
            for i in range(1, 16)
        ]

        exclusion_counts = {p.discord_id: 0 for p in players}
        MAX_CANDIDATES = 14

        if len(players) > MAX_CANDIDATES:
            def prune_priority(p):
                is_regular = p.discord_id in regular_ids
                exc_count = exclusion_counts.get(p.discord_id, 0)
                rating = p.glicko_rating or 1500.0
                return (0 if is_regular else 1, -exc_count, -rating)

            sorted_candidates = sorted(players, key=prune_priority)
            candidates_for_pool = sorted_candidates[:MAX_CANDIDATES]
            pre_excluded = sorted_candidates[MAX_CANDIDATES:]
        else:
            candidates_for_pool = players
            pre_excluded = []

        # Exactly 1 player should be pruned
        assert len(candidates_for_pool) == 14
        assert len(pre_excluded) == 1


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

    def _persist_match_state(self, guild_id, state):
        state.pending_match_id = 1234
        self.state = state

    def get_last_shuffle(self, guild_id, pending_match_id=None):
        return self.state

    def set_last_shuffle(self, guild_id, state):
        self.state = state

    def set_shuffle_message_info(self, guild_id, **kwargs):
        self.message_info = kwargs
        if self.state:
            # Mirror the real service: only overwrite fields passed as non-None
            if kwargs.get("message_id") is not None:
                self.state.shuffle_message_id = kwargs["message_id"]
            if kwargs.get("channel_id") is not None:
                self.state.shuffle_channel_id = kwargs["channel_id"]
            if kwargs.get("jump_url") is not None:
                self.state.shuffle_message_jump_url = kwargs["jump_url"]
            if kwargs.get("thread_id") is not None:
                self.state.thread_shuffle_thread_id = kwargs["thread_id"]


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

    async def test_succeeds_with_sixteen_players(self, player_repository):
        """A full 16-player lobby produces a 10-player draft awaiting choices."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 16)
        for pid in player_ids:
            player_repository.set_captain_eligible(pid, guild_id, True)

        cog = _make_draft_cog(player_repository)
        interaction = _FakeInteraction(guild_id)

        result = await cog._execute_draft(interaction, guild_id, _make_lobby(player_ids))

        assert result is True
        state = cog.draft_state_manager.get_state(guild_id)
        assert state is not None
        assert state.phase == DraftPhase.WINNER_CHOICE
        # 2 captains + 8 drafted players
        assert len(state.player_pool_ids) == 10
        assert state.captain1_id != state.captain2_id
        assert {state.captain1_id, state.captain2_id} <= set(player_ids)
        # 16 lobby players - 10 selected = 6 excluded, with no overlap
        assert len(state.excluded_player_ids) == 6
        assert not (set(state.player_pool_ids) & set(state.excluded_player_ids))
        assert state.coinflip_winner_id in (state.captain1_id, state.captain2_id)
        # excluded players get an exclusion bump so they are prioritised next draft
        excl = player_repository.get_exclusion_counts(state.excluded_player_ids, guild_id)
        assert all(excl.get(pid, 0) > 0 for pid in state.excluded_player_ids)
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

    async def test_fails_without_enough_captains(self, player_repository):
        """With fewer than 2 captain opt-ins, the draft is rejected cleanly."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 16)
        # only one player opts in as captain — two are required
        player_repository.set_captain_eligible(player_ids[0], guild_id, True)

        cog = _make_draft_cog(player_repository)
        interaction = _FakeInteraction(guild_id)

        result = await cog._execute_draft(interaction, guild_id, _make_lobby(player_ids))

        assert result is False
        # no zombie draft state is left behind
        assert cog.draft_state_manager.get_state(guild_id) is None
        # the user is told why — _execute_draft owns its own error messaging,
        # which commands/match.py relies on (it adds no message of its own).
        assert len(interaction.followup.messages) == 1
        # the message names the specific fix, pinning it to the eligibility
        # guard rather than a generic "captain" error
        assert "/draft captain yes" in interaction.followup.messages[0].content

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

    async def test_unexpected_pool_error_falls_back_and_warns(
        self, player_repository, monkeypatch, caplog
    ):
        """An unexpected error in balanced selection is logged loudly and the
        draft still completes via the exclusion-count fallback pool."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 16)
        for pid in player_ids:
            player_repository.set_captain_eligible(pid, guild_id, True)

        def _boom(*args, **kwargs):
            raise RuntimeError("balanced selection blew up")

        monkeypatch.setattr("commands.draft.BalancedShuffler.select_draft_pool", _boom)

        cog = _make_draft_cog(player_repository)
        interaction = _FakeInteraction(guild_id)
        with caplog.at_level(logging.WARNING, logger="cama_bot.commands.draft"):
            result = await cog._execute_draft(
                interaction, guild_id, _make_lobby(player_ids)
            )

        # the draft still completes via the fallback pool
        assert result is True
        state = cog.draft_state_manager.get_state(guild_id)
        assert state is not None
        assert len(state.player_pool_ids) == 10
        # the unexpected error surfaces as a warning, not buried as info
        assert any(
            rec.levelno == logging.WARNING and "unexpected" in rec.getMessage()
            for rec in caplog.records
        )


class TestHandlePlayerPick:
    """Regression tests for Discord component handling during active drafting."""

    async def test_final_pick_defers_then_edits_original_message(self, player_repository):
        """The last pick does slow match setup, so it must acknowledge the button first."""
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        captain1, captain2 = player_ids[0], player_ids[1]
        final_pick = player_ids[9]

        match_service = _FakeDraftMatchService()
        lobby_manager = MagicMock()
        bot = SimpleNamespace(
            betting_service=None,
            lobby_service=None,
            get_cog=lambda _name: None,
        )
        cog = DraftCommands(
            bot=bot,
            player_repo=player_repository,
            lobby_manager=lobby_manager,
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
        state.player_draft_first_captain_id = captain1
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

    async def test_final_pick_stores_thread_id_for_record_finalize(self, player_repository):
        """Draft completion must persist the lobby thread id into the pending
        match state, exactly as the shuffle path does.

        Regression: /record finalizes the lobby thread via
        pending_state.thread_shuffle_thread_id (record_match clears the state,
        so there is no fallback). The draft path posted to the thread but never
        stored its id, so drafted matches left their thread stuck on
        "🔒 Draft Complete - Awaiting Results" forever after recording.
        """
        guild_id = TEST_GUILD_ID
        player_ids = _register_draft_players(player_repository, guild_id, 10)
        captain1, captain2 = player_ids[0], player_ids[1]
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
                self.sent.append({"content": content, **kwargs})

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
        cog = DraftCommands(
            bot=bot,
            player_repo=player_repository,
            lobby_manager=MagicMock(),
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
        state.player_draft_first_captain_id = captain1
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

        interaction = _FakeComponentInteraction(guild_id, user_id=captain1)

        await cog.handle_player_pick(interaction, guild_id, final_pick)

        # The draft-complete embed was posted to the lobby thread
        assert thread.sent, "draft completion should post to the lobby thread"
        # The thread id must survive in the pending state so /record can
        # rename + archive the thread once the match result is in.
        assert match_service.state.thread_shuffle_thread_id == thread_id

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
        state.player_draft_first_captain_id = captain1  # captain1 picks at index 0
        state.radiant_hero_pick_order = 1
        state.dire_hero_pick_order = 2
        state.draft_channel_id = 555_000
        state.draft_message_id = 9_000_001
        state.radiant_player_ids = [captain1]
        state.dire_player_ids = [captain2]
        # captain1 owns index 0; index 1 belongs to captain2 (SNAKE_DRAFT_ORDER[1] == 1).
        assert state.current_captain_id == captain1
        assert SNAKE_DRAFT_ORDER[1] == 1

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
