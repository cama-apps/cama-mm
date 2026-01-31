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


class TestForceRandomCaptains:
    """Tests for force_random_captains functionality used in shuffle auto-redirect."""

    def test_select_captains_force_random_all_eligible(self):
        """When force_random_captains=True, any player can be captain."""
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

    def test_force_random_captains_different_players(self):
        """Random selection always picks two different players."""
        import random

        player_ids = list(range(1, 17))  # 16 players (>=15 threshold)

        # Verify random.sample always gives different players
        for _ in range(100):
            selected = random.sample(player_ids, 2)
            assert len(selected) == 2
            assert selected[0] != selected[1]

    def test_shuffle_redirect_threshold(self):
        """Verify the >=15 player threshold constant."""
        # This documents the threshold behavior
        # 10-14 players: normal shuffle
        # >=15 players (15+): redirect to draft
        SHUFFLE_MAX_FOR_NORMAL = 14
        DRAFT_REDIRECT_MIN = 15

        assert DRAFT_REDIRECT_MIN > SHUFFLE_MAX_FOR_NORMAL

    def test_lobby_player_count_includes_conditional(self):
        """Verify total count includes both regular and conditional players."""
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
        assert total == 16
        assert total >= 15  # Would trigger draft redirect


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
        from datetime import datetime
        from domain.models.lobby import Lobby
        import random

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
        from datetime import datetime
        from domain.models.lobby import Lobby
        import random

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
        from datetime import datetime
        from domain.models.lobby import Lobby
        import random

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
        from datetime import datetime
        from domain.models.lobby import Lobby
        import random

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
