"""
Standalone script to delete all fake users (discord_id < 0) from the database.
Respects the DB_PATH environment variable if set; otherwise uses the default.
"""

import os
import sys

from repositories.player_repository import PlayerRepository


def main() -> int:
    db_path = os.getenv("DB_PATH", "cama_shuffle.db")
    print(f"Using database path: {db_path}")

    try:
        player_repo = PlayerRepository(db_path)
        print("Removing fake users from database...")
        # Get all unique guild IDs that have fake users
        with player_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT DISTINCT guild_id FROM players WHERE discord_id < 0")
            guild_ids = [row["guild_id"] for row in cursor.fetchall()]

        total_deleted = 0
        for guild_id in guild_ids:
            deleted = player_repo.delete_fake_users(guild_id)
            total_deleted += deleted
            if deleted > 0:
                print(f"  Removed {deleted} fake user(s) from guild {guild_id}")

        print(f"Removed {total_deleted} fake user(s) total from the database.")
        return 0
    except Exception as exc:
        print(f"Error while removing fake users: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
