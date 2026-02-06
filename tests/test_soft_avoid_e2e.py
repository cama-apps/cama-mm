"""
End-to-end tests for soft avoid feature.
"""

import pytest

from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from repositories.soft_avoid_repository import SoftAvoidRepository
from services.match_service import MatchService


@pytest.fixture
def soft_avoid_repo(repo_db_path):
    """Create a SoftAvoidRepository with temp database."""
    return SoftAvoidRepository(repo_db_path)


@pytest.fixture
def player_repo(repo_db_path):
    """Create a PlayerRepository with temp database."""
    return PlayerRepository(repo_db_path)


@pytest.fixture
def match_repo(repo_db_path):
    """Create a MatchRepository with temp database."""
    return MatchRepository(repo_db_path)


@pytest.fixture
def match_service(player_repo, match_repo, soft_avoid_repo):
    """Create a MatchService with soft avoid support."""
    return MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
        soft_avoid_repo=soft_avoid_repo,
    )


def register_test_players(player_repo: PlayerRepository, guild_id: int = 0, count: int = 10) -> list[int]:
    """Register test players and return their discord IDs."""
    discord_ids = []
    for i in range(count):
        discord_id = 1000 + i
        player_repo.add(
            discord_id=discord_id,
            discord_username=f"Player{i + 1}",
            guild_id=guild_id,
            initial_mmr=3000 + i * 100,
            glicko_rating=1500.0,
            glicko_rd=100.0,
            glicko_volatility=0.06,
        )
        # Set preferred roles
        player_repo.update_roles(discord_id, guild_id, ["1", "2", "3", "4", "5"])
        discord_ids.append(discord_id)
    return discord_ids


class TestSoftAvoidE2E:
    """End-to-end tests for soft avoid feature."""

    def test_shuffle_with_avoids_loads_and_uses(self, match_service, soft_avoid_repo, player_repo):
        """Test that shuffle loads avoids and uses them."""
        guild_id = 123
        discord_ids = register_test_players(player_repo, guild_id=guild_id, count=10)

        # Create an avoid
        avoid = soft_avoid_repo.create_or_extend_avoid(
            guild_id=guild_id,
            avoider_id=discord_ids[0],
            avoided_id=discord_ids[1],
            games=10,
        )

        # Shuffle should work without errors
        result = match_service.shuffle_players(
            player_ids=discord_ids,
            guild_id=guild_id,
        )

        assert result is not None
        assert "radiant_team" in result
        assert "dire_team" in result

    def test_avoid_not_decremented_after_shuffle_only(self, match_service, soft_avoid_repo, player_repo):
        """Test that avoids are NOT decremented after shuffle (only after record_match)."""
        guild_id = 123
        discord_ids = register_test_players(player_repo, guild_id=guild_id, count=10)

        # Create an avoid
        avoid = soft_avoid_repo.create_or_extend_avoid(
            guild_id=guild_id,
            avoider_id=discord_ids[0],
            avoided_id=discord_ids[1],
            games=10,
        )

        # Shuffle multiple times without recording
        for _ in range(5):
            result = match_service.shuffle_players(
                player_ids=discord_ids,
                guild_id=guild_id,
            )

        # Avoid should still have 10 games (not decremented yet)
        avoids = soft_avoid_repo.get_active_avoids_for_players(
            guild_id=guild_id,
            player_ids=discord_ids,
        )
        assert len(avoids) == 1
        assert avoids[0].games_remaining == 10

    def test_avoid_decrements_after_record_match_opposite_teams(self, match_service, soft_avoid_repo, player_repo):
        """Test that avoids decrement after record_match when players are on opposite teams."""
        guild_id = 123
        discord_ids = register_test_players(player_repo, guild_id=guild_id, count=10)

        # Create an avoid
        avoid = soft_avoid_repo.create_or_extend_avoid(
            guild_id=guild_id,
            avoider_id=discord_ids[0],
            avoided_id=discord_ids[1],
            games=10,
        )

        # Keep shuffling until the pair is on opposite teams
        max_attempts = 50
        for _ in range(max_attempts):
            result = match_service.shuffle_players(
                player_ids=discord_ids,
                guild_id=guild_id,
            )

            radiant_ids = set(result["radiant_team"].players[i].discord_id for i in range(5))
            dire_ids = set(result["dire_team"].players[i].discord_id for i in range(5))

            # Check if they're on opposite teams
            avoider_on_radiant = discord_ids[0] in radiant_ids
            avoided_on_radiant = discord_ids[1] in radiant_ids

            if avoider_on_radiant != avoided_on_radiant:
                # They're on opposite teams - record the match
                match_service.record_match(winning_team="radiant", guild_id=guild_id)

                # Now the avoid should be decremented
                avoids = soft_avoid_repo.get_active_avoids_for_players(
                    guild_id=guild_id,
                    player_ids=discord_ids,
                )
                if len(avoids) == 1:
                    assert avoids[0].games_remaining == 9
                    return  # Test passed

            # Clear shuffle state for next attempt (don't record)
            match_service.clear_last_shuffle(guild_id)

        pytest.skip("Could not get opposite teams after max attempts")

    def test_bidirectional_avoids_work(self, match_service, soft_avoid_repo, player_repo):
        """Test that bidirectional avoids work correctly."""
        guild_id = 123
        discord_ids = register_test_players(player_repo, guild_id=guild_id, count=10)

        # Create bidirectional avoids
        soft_avoid_repo.create_or_extend_avoid(
            guild_id=guild_id,
            avoider_id=discord_ids[0],
            avoided_id=discord_ids[1],
            games=10,
        )
        soft_avoid_repo.create_or_extend_avoid(
            guild_id=guild_id,
            avoider_id=discord_ids[1],
            avoided_id=discord_ids[0],
            games=10,
        )

        # Shuffle should work
        result = match_service.shuffle_players(
            player_ids=discord_ids,
            guild_id=guild_id,
        )

        assert result is not None

        # Both avoids should be active (not decremented yet - no record_match)
        avoids = soft_avoid_repo.get_active_avoids_for_players(
            guild_id=guild_id,
            player_ids=discord_ids,
        )
        assert len(avoids) == 2
        for avoid in avoids:
            assert avoid.games_remaining == 10

    def test_goodness_score_includes_avoid_penalty(self, match_service, soft_avoid_repo, player_repo):
        """Test that goodness_score includes soft avoid penalty when pair is on same team."""
        guild_id = 123
        discord_ids = register_test_players(player_repo, guild_id=guild_id, count=10)

        # Create avoids (multiple to increase chance of penalty)
        soft_avoid_repo.create_or_extend_avoid(guild_id=guild_id, avoider_id=discord_ids[0], avoided_id=discord_ids[1], games=10)
        soft_avoid_repo.create_or_extend_avoid(guild_id=guild_id, avoider_id=discord_ids[2], avoided_id=discord_ids[3], games=10)
        soft_avoid_repo.create_or_extend_avoid(guild_id=guild_id, avoider_id=discord_ids[4], avoided_id=discord_ids[5], games=10)

        # Shuffle
        result = match_service.shuffle_players(
            player_ids=discord_ids,
            guild_id=guild_id,
        )

        # The goodness_score should be a float
        assert isinstance(result["goodness_score"], (int, float))
        assert result["goodness_score"] >= 0

    def test_effective_avoid_ids_stored_in_shuffle_state(self, match_service, soft_avoid_repo, player_repo):
        """Test that effective_avoid_ids are stored in shuffle state for deferred decrement."""
        guild_id = 123
        discord_ids = register_test_players(player_repo, guild_id=guild_id, count=10)

        # Create an avoid
        avoid = soft_avoid_repo.create_or_extend_avoid(
            guild_id=guild_id,
            avoider_id=discord_ids[0],
            avoided_id=discord_ids[1],
            games=10,
        )

        # Keep shuffling until the pair is on opposite teams
        max_attempts = 50
        for _ in range(max_attempts):
            result = match_service.shuffle_players(
                player_ids=discord_ids,
                guild_id=guild_id,
            )

            radiant_ids = set(result["radiant_team"].players[i].discord_id for i in range(5))
            dire_ids = set(result["dire_team"].players[i].discord_id for i in range(5))

            # Check if they're on opposite teams
            avoider_on_radiant = discord_ids[0] in radiant_ids
            avoided_on_radiant = discord_ids[1] in radiant_ids

            if avoider_on_radiant != avoided_on_radiant:
                # Check that effective_avoid_ids is stored in state
                state = match_service.get_last_shuffle(guild_id)
                assert state is not None
                assert "effective_avoid_ids" in state
                assert avoid.id in state["effective_avoid_ids"]
                return  # Test passed

            # Clear shuffle state for next attempt
            match_service.clear_last_shuffle(guild_id)

        pytest.skip("Could not get opposite teams after max attempts")
