"""
Tests for Immortal Draft functionality.
"""

import pytest

from domain.models.draft import DraftPhase, DraftState, SNAKE_DRAFT_ORDER
from domain.services.draft_service import DraftService
from repositories.player_repository import PlayerRepository
from services.draft_state_manager import DraftStateManager


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


class TestCaptainEligibility:
    """Tests for captain eligibility repository methods."""

    def test_set_captain_eligible_true(self, player_repository: PlayerRepository):
        """Player can be set as captain-eligible."""
        # Add a player first
        player_repository.add(
            discord_id=1001,
            discord_username="TestPlayer",
            initial_mmr=3000,
        )

        # Set as captain-eligible
        result = player_repository.set_captain_eligible(1001, True)
        assert result is True

        # Verify eligibility
        assert player_repository.get_captain_eligible(1001) is True

    def test_set_captain_eligible_false(self, player_repository: PlayerRepository):
        """Player can be set as not captain-eligible."""
        # Add a player first
        player_repository.add(
            discord_id=1002,
            discord_username="TestPlayer2",
            initial_mmr=3000,
        )

        # Set as captain-eligible first
        player_repository.set_captain_eligible(1002, True)
        assert player_repository.get_captain_eligible(1002) is True

        # Remove eligibility
        result = player_repository.set_captain_eligible(1002, False)
        assert result is True
        assert player_repository.get_captain_eligible(1002) is False

    def test_get_captain_eligible_default_false(self, player_repository: PlayerRepository):
        """New players default to not captain-eligible."""
        player_repository.add(
            discord_id=1003,
            discord_username="TestPlayer3",
            initial_mmr=3000,
        )

        # Should default to False
        assert player_repository.get_captain_eligible(1003) is False

    def test_get_captain_eligible_nonexistent_player(self, player_repository: PlayerRepository):
        """Non-existent player returns False for captain eligibility."""
        assert player_repository.get_captain_eligible(9999) is False

    def test_set_captain_eligible_nonexistent_player(self, player_repository: PlayerRepository):
        """Setting eligibility for non-existent player returns False."""
        result = player_repository.set_captain_eligible(9999, True)
        assert result is False

    def test_get_captain_eligible_players(self, player_repository: PlayerRepository):
        """Get list of captain-eligible players from a set of IDs."""
        # Add several players
        for i in range(1, 6):
            player_repository.add(
                discord_id=2000 + i,
                discord_username=f"Player{i}",
                initial_mmr=3000 + i * 100,
            )

        # Set some as captain-eligible
        player_repository.set_captain_eligible(2001, True)
        player_repository.set_captain_eligible(2003, True)
        player_repository.set_captain_eligible(2005, True)

        # Query subset of players
        all_ids = [2001, 2002, 2003, 2004, 2005]
        eligible = player_repository.get_captain_eligible_players(all_ids)

        assert sorted(eligible) == [2001, 2003, 2005]

    def test_get_captain_eligible_players_empty_list(self, player_repository: PlayerRepository):
        """Empty input list returns empty result."""
        result = player_repository.get_captain_eligible_players([])
        assert result == []

    def test_get_captain_eligible_players_none_eligible(self, player_repository: PlayerRepository):
        """If no players are eligible, returns empty list."""
        # Add players but don't set any as eligible
        for i in range(1, 4):
            player_repository.add(
                discord_id=3000 + i,
                discord_username=f"Player{i}",
                initial_mmr=3000,
            )

        eligible = player_repository.get_captain_eligible_players([3001, 3002, 3003])
        assert eligible == []

    def test_get_captain_eligible_players_subset(self, player_repository: PlayerRepository):
        """Only returns eligible players from the requested subset."""
        # Add players
        for i in range(1, 6):
            player_repository.add(
                discord_id=4000 + i,
                discord_username=f"Player{i}",
                initial_mmr=3000,
            )

        # Set players 1, 2, 3 as eligible
        player_repository.set_captain_eligible(4001, True)
        player_repository.set_captain_eligible(4002, True)
        player_repository.set_captain_eligible(4003, True)

        # Only query for 2 and 4 - should return only 2
        eligible = player_repository.get_captain_eligible_players([4002, 4004])
        assert eligible == [4002]
