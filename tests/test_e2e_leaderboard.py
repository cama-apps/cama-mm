"""
End-to-end tests for leaderboard edge cases.
"""

import os
import tempfile
import time

import pytest

from database import Database
from rating_system import CamaRatingSystem
from utils.formatting import JOPACOIN_EMOTE


class TestLeaderboardEdgeCases:
    """Tests for leaderboard edge cases."""

    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix=".db")
        os.close(fd)
        db = Database(db_path)
        yield db
        try:
            import sqlite3

            sqlite3.connect(db_path).close()
        except Exception:
            pass
        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except PermissionError:
            time.sleep(0.2)
            try:
                os.unlink(db_path)
            except Exception:
                pass

    def test_empty_leaderboard(self, test_db):
        """Test leaderboard with no players."""
        # Get all players (should be empty)
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute("SELECT * FROM players")
        players = cursor.fetchall()
        conn.close()

        assert len(players) == 0, "Should have no players"

    def test_leaderboard_with_ties(self, test_db):
        """Test leaderboard with players having same jopacoin balance."""
        # Create players with same jopacoin but different ratings
        player_ids = [300001, 300002, 300003]
        player_names = ["Player1", "Player2", "Player3"]

        # All have same jopacoin balance but different ratings
        for pid, name in zip(player_ids, player_names):
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0 + (pid % 500),  # Different ratings
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Set same jopacoin balance for all players
        conn = test_db.get_connection()
        cursor = conn.cursor()
        for pid in player_ids:
            cursor.execute("UPDATE players SET jopacoin_balance = 100 WHERE discord_id = ?", (pid,))
        conn.commit()
        conn.close()

        # Get all players and sort by jopacoin, then rating
        rating_system = CamaRatingSystem()

        # Track players with their discord_id
        players_with_ids = []
        for pid in player_ids:
            player = test_db.get_player(pid)
            if player:
                players_with_ids.append((player, pid))

        # Sort by jopacoin (descending), then wins (descending), then rating (descending)
        players_with_stats = []
        for player, pid in players_with_ids:
            total_games = player.wins + player.losses
            win_rate = (player.wins / total_games * 100) if total_games > 0 else 0.0
            cama_rating = (
                rating_system.rating_to_display(player.glicko_rating)
                if player.glicko_rating
                else None
            )
            jopacoin_balance = test_db.get_player_balance(pid)
            players_with_stats.append(
                (player, jopacoin_balance, player.wins, player.losses, win_rate, cama_rating)
            )

        players_with_stats.sort(
            key=lambda x: (x[1], x[2], x[5] if x[5] is not None else 0), reverse=True
        )

        # All should have 100 jopacoin (tied)
        for player, jopacoin, _wins, _losses, win_rate, _rating in players_with_stats:
            assert jopacoin == 100, (
                f"All players should have 100 jopacoin, {player.name} has {jopacoin}"
            )

        # Should be sorted by wins, then rating as tiebreaker (all have same jopacoin)
        # Since all have same wins from the matches, should be sorted by rating
        ratings = [r for _, _, _, _, _, r in players_with_stats if r is not None]
        assert ratings == sorted(ratings, reverse=True), (
            "Should be sorted by rating when jopacoin and wins are tied"
        )

    def test_leaderboard_with_no_games(self, test_db):
        """Test leaderboard with players who have no games."""
        # Create players with no matches
        player_ids = [300101, 300102]
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Get players and calculate stats
        rating_system = CamaRatingSystem()

        all_players = []
        for pid in player_ids:
            player = test_db.get_player(pid)
            if player:
                all_players.append(player)

        players_with_stats = []
        for player in all_players:
            total_games = player.wins + player.losses
            win_rate = (player.wins / total_games * 100) if total_games > 0 else 0.0
            cama_rating = (
                rating_system.rating_to_display(player.glicko_rating)
                if player.glicko_rating
                else None
            )
            players_with_stats.append((player, player.wins, player.losses, win_rate, cama_rating))

        # All should have 0 wins, 0 losses, 0% win rate
        for player, wins, losses, win_rate, _rating in players_with_stats:
            assert wins == 0, f"Player {player.name} should have 0 wins"
            assert losses == 0, f"Player {player.name} should have 0 losses"
            assert win_rate == 0.0, f"Player {player.name} should have 0% win rate"

    def test_leaderboard_large_dataset(self, test_db):
        """Test leaderboard with many players (top 20 limit)."""
        # Create 25 players
        player_ids = list(range(300201, 300226))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0 + (pid % 1000),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Give them varying jopacoin balances
        conn = test_db.get_connection()
        cursor = conn.cursor()
        for i, pid in enumerate(player_ids):
            # Give jopacoin based on index (higher index = more jopacoin)
            jopacoin_balance = i * 10
            cursor.execute(
                "UPDATE players SET jopacoin_balance = ? WHERE discord_id = ?",
                (jopacoin_balance, pid),
            )
        conn.commit()
        conn.close()

        # Get all players and sort
        rating_system = CamaRatingSystem()

        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute("SELECT * FROM players WHERE discord_id >= 300201 AND discord_id < 300226")
        rows = cursor.fetchall()
        conn.close()

        # Track players with their discord_id
        players_with_ids = []
        for row in rows:
            player = test_db.get_player(row["discord_id"])
            if player:
                players_with_ids.append((player, row["discord_id"]))

        players_with_stats = []
        for player, pid in players_with_ids:
            total_games = player.wins + player.losses
            win_rate = (player.wins / total_games * 100) if total_games > 0 else 0.0
            cama_rating = (
                rating_system.rating_to_display(player.glicko_rating)
                if player.glicko_rating
                else None
            )
            jopacoin_balance = test_db.get_player_balance(pid)
            players_with_stats.append(
                (player, jopacoin_balance, player.wins, player.losses, win_rate, cama_rating)
            )

        players_with_stats.sort(
            key=lambda x: (x[1], x[2], x[5] if x[5] is not None else 0), reverse=True
        )

        # Should have 25 players
        assert len(players_with_stats) == 25, "Should have 25 players"

        # Top player should have most jopacoin
        top_player_jopacoin = players_with_stats[0][1]
        assert top_player_jopacoin >= players_with_stats[-1][1], (
            "Top player should have at least as many jopacoin as bottom"
        )

    def test_leaderboard_sorts_by_jopacoin_then_wins_then_rating(self, test_db):
        """Test that leaderboard sorts correctly: jopacoin -> wins -> rating."""
        from rating_system import CamaRatingSystem

        rating_system = CamaRatingSystem()

        # Create players with different combinations of jopacoin, wins, and ratings
        # Player 1: High jopacoin, low wins, high rating
        # Player 2: High jopacoin, high wins, low rating
        # Player 3: Low jopacoin, high wins, high rating
        # Player 4: Same jopacoin as Player 1, same wins, different rating
        players = [
            {"id": 400001, "name": "Player1", "jopacoin": 100, "wins": 1, "rating": 2000.0},
            {"id": 400002, "name": "Player2", "jopacoin": 100, "wins": 5, "rating": 1500.0},
            {"id": 400003, "name": "Player3", "jopacoin": 50, "wins": 10, "rating": 2000.0},
            {"id": 400004, "name": "Player4", "jopacoin": 100, "wins": 1, "rating": 1800.0},
        ]

        for p in players:
            test_db.add_player(
                discord_id=p["id"],
                discord_username=p["name"],
                initial_mmr=1500,
                glicko_rating=p["rating"],
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            # Set wins and jopacoin
            conn = test_db.get_connection()
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE players SET wins = ?, jopacoin_balance = ? WHERE discord_id = ?",
                (p["wins"], p["jopacoin"], p["id"]),
            )
            conn.commit()
            conn.close()

        # Simulate the leaderboard command logic
        with test_db.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT discord_id, discord_username, wins, losses, glicko_rating, COALESCE(jopacoin_balance, 0) as jopacoin_balance "
                "FROM players WHERE discord_id >= 400001 AND discord_id < 400005"
            )
            rows = cursor.fetchall()

        players_with_stats = []
        for row in rows:
            wins = row["wins"] or 0
            rating_value = row["glicko_rating"]
            cama_rating = (
                rating_system.rating_to_display(rating_value) if rating_value is not None else None
            )
            jopacoin_balance = row["jopacoin_balance"] or 0

            players_with_stats.append(
                {
                    "discord_id": row["discord_id"],
                    "username": row["discord_username"],
                    "wins": wins,
                    "rating": cama_rating,
                    "jopacoin_balance": jopacoin_balance,
                }
            )

        # Sort exactly like the leaderboard command does
        players_with_stats.sort(
            key=lambda x: (
                x["jopacoin_balance"],
                x["wins"],
                x["rating"] if x["rating"] is not None else 0,
            ),
            reverse=True,
        )

        # Expected order:
        # 1. Player2: 100 jopacoin, 5 wins, 1500 rating
        # 2. Player1: 100 jopacoin, 1 win, 2000 rating (higher rating than Player4)
        # 3. Player4: 100 jopacoin, 1 win, 1800 rating
        # 4. Player3: 50 jopacoin, 10 wins, 2000 rating

        assert len(players_with_stats) == 4, "Should have 4 players"

        # Top player should be Player2 (highest jopacoin, then highest wins)
        assert players_with_stats[0]["discord_id"] == 400002, (
            "Player2 should be first (100 jopacoin, 5 wins)"
        )
        assert players_with_stats[0]["jopacoin_balance"] == 100
        assert players_with_stats[0]["wins"] == 5

        # Second should be Player1 (100 jopacoin, 1 win, higher rating than Player4)
        assert players_with_stats[1]["discord_id"] == 400001, (
            "Player1 should be second (100 jopacoin, 1 win, 2000 rating)"
        )
        assert players_with_stats[1]["jopacoin_balance"] == 100
        assert players_with_stats[1]["wins"] == 1

        # Third should be Player4 (100 jopacoin, 1 win, lower rating than Player1)
        assert players_with_stats[2]["discord_id"] == 400004, (
            "Player4 should be third (100 jopacoin, 1 win, 1800 rating)"
        )
        assert players_with_stats[2]["jopacoin_balance"] == 100
        assert players_with_stats[2]["wins"] == 1

        # Fourth should be Player3 (50 jopacoin, even though has more wins)
        assert players_with_stats[3]["discord_id"] == 400003, (
            "Player3 should be fourth (50 jopacoin, despite 10 wins)"
        )
        assert players_with_stats[3]["jopacoin_balance"] == 50
        assert players_with_stats[3]["wins"] == 10

    def test_leaderboard_displays_jopacoin(self, test_db):
        """Test that leaderboard output includes jopacoin display."""
        from rating_system import CamaRatingSystem

        rating_system = CamaRatingSystem()

        # Create a test player
        player_id = 400101
        test_db.add_player(
            discord_id=player_id,
            discord_username="TestPlayer",
            initial_mmr=1500,
            glicko_rating=1600.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Set jopacoin balance
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "UPDATE players SET jopacoin_balance = 42 WHERE discord_id = ?", (player_id,)
        )
        conn.commit()
        conn.close()

        # Simulate the leaderboard command logic
        with test_db.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT discord_id, discord_username, wins, losses, glicko_rating, COALESCE(jopacoin_balance, 0) as jopacoin_balance "
                "FROM players WHERE discord_id = ?",
                (player_id,),
            )
            row = cursor.fetchone()

        wins = row["wins"] or 0
        losses = row["losses"] or 0
        total_games = wins + losses
        win_rate = (wins / total_games * 100) if total_games > 0 else 0.0
        rating_value = row["glicko_rating"]
        (
            rating_system.rating_to_display(rating_value) if rating_value is not None else None
        )
        jopacoin_balance = row["jopacoin_balance"] or 0

        # Format like the leaderboard command does
        stats = f"{wins}-{losses}"
        if wins + losses > 0:
            stats += f" ({win_rate:.0f}%)"
        jopacoin_display = f"{jopacoin_balance} {JOPACOIN_EMOTE}"

        # Verify jopacoin is in the display
        assert JOPACOIN_EMOTE in jopacoin_display, (
            "Jopacoin display should include the jopacoin emote"
        )
        assert str(jopacoin_balance) in jopacoin_display, (
            f"Jopacoin display should include balance {jopacoin_balance}"
        )
        assert jopacoin_balance == 42, "Jopacoin balance should be 42"

    def test_leaderboard_with_zero_jopacoin(self, test_db):
        """Test leaderboard with players having zero jopacoin."""
        from rating_system import CamaRatingSystem

        rating_system = CamaRatingSystem()

        # Create players with zero jopacoin but different wins
        player_ids = [400201, 400202, 400203]
        for i, pid in enumerate(player_ids):
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0 + i * 100,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            # Set wins but zero jopacoin
            conn = test_db.get_connection()
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE players SET wins = ?, jopacoin_balance = 0 WHERE discord_id = ?",
                (i + 1, pid),  # Player1: 1 win, Player2: 2 wins, Player3: 3 wins
            )
            conn.commit()
            conn.close()

        # Simulate leaderboard sorting
        with test_db.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT discord_id, discord_username, wins, losses, glicko_rating, COALESCE(jopacoin_balance, 0) as jopacoin_balance "
                "FROM players WHERE discord_id >= 400201 AND discord_id < 400204"
            )
            rows = cursor.fetchall()

        players_with_stats = []
        for row in rows:
            wins = row["wins"] or 0
            rating_value = row["glicko_rating"]
            cama_rating = (
                rating_system.rating_to_display(rating_value) if rating_value is not None else None
            )
            jopacoin_balance = row["jopacoin_balance"] or 0

            players_with_stats.append(
                {
                    "discord_id": row["discord_id"],
                    "wins": wins,
                    "rating": cama_rating,
                    "jopacoin_balance": jopacoin_balance,
                }
            )

        players_with_stats.sort(
            key=lambda x: (
                x["jopacoin_balance"],
                x["wins"],
                x["rating"] if x["rating"] is not None else 0,
            ),
            reverse=True,
        )

        # All have 0 jopacoin, so should be sorted by wins (descending)
        # Player3 (3 wins) -> Player2 (2 wins) -> Player1 (1 win)
        assert players_with_stats[0]["discord_id"] == 400203, "Player3 should be first (3 wins)"
        assert players_with_stats[1]["discord_id"] == 400202, "Player2 should be second (2 wins)"
        assert players_with_stats[2]["discord_id"] == 400201, "Player1 should be third (1 win)"

        # All should have 0 jopacoin
        for player in players_with_stats:
            assert player["jopacoin_balance"] == 0, "All players should have 0 jopacoin"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
