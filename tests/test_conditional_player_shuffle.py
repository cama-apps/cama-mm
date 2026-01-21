"""
Tests for conditional player (frogling) inclusion logic during shuffle.

Verifies that:
- Regular players are ALWAYS included when regular_count <= 10
- Conditional players only fill remaining slots (10 - regular_count)
- Conditional players are prioritized by rating (higher = first) then RD (lower = first)
- Extra conditional players are excluded, not regular players
"""

import pytest

from domain.models.player import Player


def priority_key(player: Player) -> tuple[float, float]:
    """
    Priority key for conditional player selection.
    Higher rating = higher priority, lower RD = higher priority.
    Extracted from commands/match.py for testing.
    """
    rating = player.glicko_rating if player.glicko_rating else 1500.0
    rd = player.glicko_rd if player.glicko_rd else 350.0
    return (rating, -rd)


def select_players_for_shuffle(
    regular_player_ids: list[int],
    regular_players: list[Player],
    conditional_player_ids: list[int],
    conditional_players: list[Player],
) -> tuple[list[int], list[Player], list[int], list[int]]:
    """
    Select players for shuffle, matching the logic in commands/match.py.

    Returns:
        (player_ids, players, included_conditional_ids, excluded_conditional_ids)
    """
    regular_count = len(regular_player_ids)

    # Start with all regular players
    player_ids = list(regular_player_ids)
    players = list(regular_players)
    included_conditional_ids = []
    excluded_conditional_ids = []

    if regular_count < 10:
        # Need to include some conditional players to reach exactly 10
        conditional_pairs = list(zip(conditional_player_ids, conditional_players))
        conditional_pairs.sort(key=lambda x: priority_key(x[1]), reverse=True)

        # Take exactly enough to reach 10
        slots_available = 10 - regular_count
        for cid, cplayer in conditional_pairs[:slots_available]:
            player_ids.append(cid)
            players.append(cplayer)
            included_conditional_ids.append(cid)

        # All remaining are excluded
        excluded_conditional_ids = [cid for cid, _ in conditional_pairs[slots_available:]]
    else:
        # 10+ regular players means all conditional are excluded
        excluded_conditional_ids = list(conditional_player_ids)

    return player_ids, players, included_conditional_ids, excluded_conditional_ids


def make_player(name: str, rating: float | None = 1500.0, rd: float | None = 350.0) -> Player:
    """Helper to create a Player with rating/RD."""
    return Player(
        name=name,
        mmr=3000,
        wins=0,
        losses=0,
        preferred_roles=["1", "2", "3", "4", "5"],
        main_role="1",
        glicko_rating=rating,
        glicko_rd=rd,
        glicko_volatility=0.06,
        discord_id=None,
        jopacoin_balance=0,
    )


class TestConditionalPlayerSelection:
    """Test that regular players are always included when regular_count <= 10."""

    def test_9_regular_1_conditional_needed(self):
        """9 regular + 2 conditional: all 9 regular play, 1 conditional plays, 1 excluded."""
        regular_ids = [100 + i for i in range(9)]
        regular_players = [make_player(f"Regular{i}") for i in range(9)]

        conditional_ids = [200, 201]
        conditional_players = [
            make_player("ConditionalLow", rating=1400.0),
            make_player("ConditionalHigh", rating=1600.0),
        ]

        player_ids, players, included, excluded = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        # Should have exactly 10 players
        assert len(player_ids) == 10
        assert len(players) == 10

        # All 9 regular should be included
        for rid in regular_ids:
            assert rid in player_ids

        # Only the higher-rated conditional should be included
        assert 201 in included  # ConditionalHigh (1600)
        assert 200 in excluded  # ConditionalLow (1400)

    def test_9_regular_5_conditional_only_1_needed(self):
        """9 regular + 5 conditional: all 9 regular play, 1 conditional plays, 4 excluded."""
        regular_ids = [100 + i for i in range(9)]
        regular_players = [make_player(f"Regular{i}") for i in range(9)]

        # 5 conditional players with varying ratings
        conditional_ids = [200, 201, 202, 203, 204]
        conditional_players = [
            make_player("C1", rating=1300.0),
            make_player("C2", rating=1500.0),
            make_player("C3", rating=1700.0),  # Highest - should be included
            make_player("C4", rating=1400.0),
            make_player("C5", rating=1200.0),
        ]

        player_ids, players, included, excluded = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        assert len(player_ids) == 10
        assert len(included) == 1
        assert len(excluded) == 4

        # Only the highest-rated conditional (C3) should be included
        assert 202 in included
        assert set(excluded) == {200, 201, 203, 204}

    def test_8_regular_3_conditional_2_needed(self):
        """8 regular + 3 conditional: all 8 regular play, 2 conditional play, 1 excluded."""
        regular_ids = [100 + i for i in range(8)]
        regular_players = [make_player(f"Regular{i}") for i in range(8)]

        conditional_ids = [200, 201, 202]
        conditional_players = [
            make_player("CLow", rating=1300.0),
            make_player("CMid", rating=1500.0),
            make_player("CHigh", rating=1700.0),
        ]

        player_ids, players, included, excluded = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        assert len(player_ids) == 10
        assert len(included) == 2
        assert len(excluded) == 1

        # Top 2 by rating should be included
        assert 202 in included  # CHigh
        assert 201 in included  # CMid
        assert 200 in excluded  # CLow

    def test_10_regular_no_conditional_needed(self):
        """10 regular + 2 conditional: all 10 regular play, 2 conditional excluded."""
        regular_ids = [100 + i for i in range(10)]
        regular_players = [make_player(f"Regular{i}") for i in range(10)]

        conditional_ids = [200, 201]
        conditional_players = [
            make_player("C1", rating=1800.0),
            make_player("C2", rating=1900.0),
        ]

        player_ids, players, included, excluded = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        # All 10 regular, no conditional
        assert len(player_ids) == 10
        assert len(included) == 0
        assert set(excluded) == {200, 201}

    def test_12_regular_2_conditional_all_conditional_excluded(self):
        """12 regular + 2 conditional: shuffler will exclude 2 regular, all conditional excluded."""
        regular_ids = [100 + i for i in range(12)]
        regular_players = [make_player(f"Regular{i}") for i in range(12)]

        conditional_ids = [200, 201]
        conditional_players = [
            make_player("C1", rating=1800.0),
            make_player("C2", rating=1900.0),
        ]

        player_ids, players, included, excluded = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        # All 12 regular go to shuffler (shuffler will exclude 2)
        assert len(player_ids) == 12
        assert len(included) == 0
        assert set(excluded) == {200, 201}


class TestConditionalPlayerPriority:
    """Test that conditional players are selected by rating then RD."""

    def test_priority_by_rating(self):
        """Higher rating wins."""
        regular_ids = [100 + i for i in range(9)]
        regular_players = [make_player(f"Regular{i}") for i in range(9)]

        conditional_ids = [200, 201, 202]
        conditional_players = [
            make_player("C1", rating=1400.0, rd=350.0),
            make_player("C2", rating=1600.0, rd=350.0),  # Should win
            make_player("C3", rating=1500.0, rd=350.0),
        ]

        _, _, included, _ = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        assert included == [201]

    def test_priority_by_rd_when_rating_equal(self):
        """Lower RD wins when ratings are equal."""
        regular_ids = [100 + i for i in range(9)]
        regular_players = [make_player(f"Regular{i}") for i in range(9)]

        conditional_ids = [200, 201, 202]
        conditional_players = [
            make_player("C1", rating=1500.0, rd=200.0),  # Should win (lower RD)
            make_player("C2", rating=1500.0, rd=350.0),
            make_player("C3", rating=1500.0, rd=300.0),
        ]

        _, _, included, _ = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        assert included == [200]

    def test_priority_none_values_use_defaults(self):
        """None rating/RD uses defaults (1500.0 and 350.0)."""
        regular_ids = [100 + i for i in range(9)]
        regular_players = [make_player(f"Regular{i}") for i in range(9)]

        conditional_ids = [200, 201]
        conditional_players = [
            make_player("C1", rating=None, rd=None),  # Defaults to 1500.0, 350.0
            make_player("C2", rating=1600.0, rd=350.0),  # Should win
        ]

        _, _, included, excluded = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        assert 201 in included
        assert 200 in excluded


class TestExactly10PlayersToShuffler:
    """Test that exactly 10 players are sent to shuffler when regular_count < 10."""

    def test_exactly_10_with_9_regular(self):
        """9 regular + N conditional = exactly 10 to shuffler."""
        regular_ids = [100 + i for i in range(9)]
        regular_players = [make_player(f"Regular{i}") for i in range(9)]

        conditional_ids = [200, 201, 202, 203, 204]
        conditional_players = [make_player(f"C{i}") for i in range(5)]

        player_ids, _, _, _ = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        assert len(player_ids) == 10

    def test_exactly_10_with_8_regular(self):
        """8 regular + N conditional = exactly 10 to shuffler."""
        regular_ids = [100 + i for i in range(8)]
        regular_players = [make_player(f"Regular{i}") for i in range(8)]

        conditional_ids = [200, 201, 202, 203, 204]
        conditional_players = [make_player(f"C{i}") for i in range(5)]

        player_ids, _, _, _ = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        assert len(player_ids) == 10

    def test_exactly_10_with_7_regular(self):
        """7 regular + N conditional = exactly 10 to shuffler."""
        regular_ids = [100 + i for i in range(7)]
        regular_players = [make_player(f"Regular{i}") for i in range(7)]

        conditional_ids = [200, 201, 202, 203, 204, 205]
        conditional_players = [make_player(f"C{i}") for i in range(6)]

        player_ids, _, included, excluded = select_players_for_shuffle(
            regular_ids, regular_players, conditional_ids, conditional_players
        )

        assert len(player_ids) == 10
        assert len(included) == 3
        assert len(excluded) == 3
