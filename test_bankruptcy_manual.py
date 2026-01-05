"""
Quick script to manually test bankruptcy feature with fake users.
Run this to put a fake player into bankruptcy state.
"""

import sqlite3
import time

DB_PATH = "cama_shuffle.db"

def add_bankruptcy_to_fake_player():
    """Add bankruptcy penalty to the first fake player for testing."""
    conn = sqlite3.connect(DB_PATH)
    cursor = conn.cursor()

    # Find first fake player (negative ID)
    cursor.execute("SELECT discord_id, discord_username FROM players WHERE discord_id < 0 LIMIT 1")
    row = cursor.fetchone()

    if not row:
        print("ERROR: No fake players found. Run /addfake first!")
        conn.close()
        return

    discord_id, username = row
    print(f"Found fake player: {username} (ID: {discord_id})")

    # Set them to have debt
    cursor.execute("UPDATE players SET jopacoin_balance = -100 WHERE discord_id = ?", (discord_id,))
    print(f"SUCCESS: Set {username} balance to -100 (in debt)")

    # Declare bankruptcy (gives them penalty games)
    now = int(time.time())
    penalty_games = 5  # They'll have tombstone for 5 games

    cursor.execute("""
        INSERT INTO bankruptcy_state (discord_id, last_bankruptcy_at, penalty_games_remaining)
        VALUES (?, ?, ?)
        ON CONFLICT(discord_id) DO UPDATE SET
            last_bankruptcy_at = excluded.last_bankruptcy_at,
            penalty_games_remaining = excluded.penalty_games_remaining
    """, (discord_id, now, penalty_games))

    # Reset balance to 0 (bankruptcy clears debt)
    cursor.execute("UPDATE players SET jopacoin_balance = 0 WHERE discord_id = ?", (discord_id,))

    conn.commit()
    conn.close()

    print(f"SUCCESS: Declared bankruptcy for {username}")
    print(f"   - Balance reset to 0")
    print(f"   - Penalty games: {penalty_games}")
    print(f"   - They should now show tombstone emoji in lobby/matches!")
    print(f"\nPlayer ID: {discord_id}")

if __name__ == "__main__":
    add_bankruptcy_to_fake_player()
