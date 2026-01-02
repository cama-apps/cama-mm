"""
Recalculate and repair players.wins / players.losses from match_participants.

Why this works:
- The authoritative per-player outcome is stored in match_participants.won (0/1).
- players.wins / players.losses are derived counters and can be recomputed.

Usage:
  python fix_wins_losses.py --dry-run
  python fix_wins_losses.py
  python fix_wins_losses.py --db-path path/to/cama_shuffle.db
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from datetime import datetime
from typing import Dict, List, Tuple

from database import Database


def _default_db_path() -> str:
    return os.getenv("DB_PATH", "cama_shuffle.db")


def _make_backup(db_path: str) -> str:
    """
    Create a timestamped backup next to the db file.
    Returns the backup path.
    """
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    backup_path = f"{db_path}.backup.{ts}"
    shutil.copy2(db_path, backup_path)
    return backup_path


def _fetch_recalc_rows(conn) -> List[Dict]:
    """
    Returns one row per player, including old and recomputed wins/losses.
    """
    cursor = conn.cursor()
    cursor.execute(
        """
        WITH agg AS (
            SELECT
                discord_id,
                SUM(CASE WHEN won = 1 THEN 1 ELSE 0 END) AS new_wins,
                SUM(CASE WHEN won = 0 THEN 1 ELSE 0 END) AS new_losses
            FROM match_participants
            GROUP BY discord_id
        )
        SELECT
            p.discord_id AS discord_id,
            p.discord_username AS discord_username,
            COALESCE(p.wins, 0) AS old_wins,
            COALESCE(p.losses, 0) AS old_losses,
            COALESCE(a.new_wins, 0) AS new_wins,
            COALESCE(a.new_losses, 0) AS new_losses
        FROM players p
        LEFT JOIN agg a ON a.discord_id = p.discord_id
        ORDER BY p.discord_id
        """
    )
    return [dict(r) for r in cursor.fetchall()]


def _fetch_validation_stats(conn) -> Dict[str, int]:
    cursor = conn.cursor()
    stats: Dict[str, int] = {}

    cursor.execute("SELECT COUNT(*) AS c FROM match_participants")
    stats["match_participants_total"] = int(cursor.fetchone()["c"])

    cursor.execute("SELECT COUNT(*) AS c FROM match_participants WHERE won IN (0, 1)")
    stats["match_participants_counted"] = int(cursor.fetchone()["c"])

    cursor.execute("SELECT COUNT(*) AS c FROM match_participants WHERE won IS NULL")
    stats["match_participants_won_null"] = int(cursor.fetchone()["c"])

    cursor.execute(
        """
        SELECT COUNT(*) AS c
        FROM match_participants mp
        LEFT JOIN players p ON p.discord_id = mp.discord_id
        WHERE p.discord_id IS NULL
        """
    )
    stats["match_participants_orphaned"] = int(cursor.fetchone()["c"])

    cursor.execute("SELECT COUNT(*) AS c FROM players")
    stats["players_total"] = int(cursor.fetchone()["c"])

    return stats


def _fetch_orphaned_participants(conn) -> List[Tuple[int, int]]:
    cursor = conn.cursor()
    cursor.execute(
        """
        SELECT mp.discord_id AS discord_id, COUNT(*) AS c
        FROM match_participants mp
        LEFT JOIN players p ON p.discord_id = mp.discord_id
        WHERE p.discord_id IS NULL
        GROUP BY mp.discord_id
        ORDER BY c DESC, discord_id ASC
        """
    )
    return [(int(r["discord_id"]), int(r["c"])) for r in cursor.fetchall()]


def _print_report(rows: List[Dict], *, verbose: bool) -> int:
    changed = [r for r in rows if (r["old_wins"], r["old_losses"]) != (r["new_wins"], r["new_losses"])]

    print(f"Players total: {len(rows)}")
    print(f"Players needing update: {len(changed)}")

    if verbose:
        for r in changed:
            print(
                f"- {r['discord_id']} ({r['discord_username']}): "
                f"{r['old_wins']}-{r['old_losses']} -> {r['new_wins']}-{r['new_losses']}"
            )

    return len(changed)


def _apply_updates(conn, rows: List[Dict]) -> int:
    """
    Apply updates for all players whose wins/losses differ from recomputed values.
    Returns number of players updated.
    """
    cursor = conn.cursor()
    updated = 0
    for r in rows:
        old_pair = (r["old_wins"], r["old_losses"])
        new_pair = (r["new_wins"], r["new_losses"])
        if old_pair == new_pair:
            continue
        cursor.execute(
            """
            UPDATE players
            SET wins = ?, losses = ?, updated_at = CURRENT_TIMESTAMP
            WHERE discord_id = ?
            """,
            (int(r["new_wins"]), int(r["new_losses"]), int(r["discord_id"])),
        )
        updated += 1
    return updated


def main(argv: List[str]) -> int:
    parser = argparse.ArgumentParser(description="Recalculate players.wins/losses from match_participants.")
    parser.add_argument("--db-path", default=_default_db_path(), help="Path to SQLite DB (default: DB_PATH or cama_shuffle.db)")
    parser.add_argument("--dry-run", action="store_true", help="Preview changes without writing anything")
    parser.add_argument("--verbose", action="store_true", help="Print per-player changes")
    args = parser.parse_args(argv)

    db_path = args.db_path
    if not os.path.exists(db_path):
        print(f"ERROR: Database file not found: {db_path}", file=sys.stderr)
        return 2

    # Note: Database() will run schema initialization/migrations (idempotent).
    db = Database(db_path=db_path)

    with db.connection() as conn:
        stats = _fetch_validation_stats(conn)
        rows = _fetch_recalc_rows(conn)

        # Validate computed totals
        computed_total = sum(int(r["new_wins"]) + int(r["new_losses"]) for r in rows)
        counted = stats["match_participants_counted"]
        if computed_total != counted:
            print(
                "WARNING: computed total (sum of per-player wins+losses) does not match "
                f"counted match_participants rows (won in 0/1): {computed_total} != {counted}"
            )

        if stats["match_participants_won_null"] > 0:
            print(f"WARNING: {stats['match_participants_won_null']} match_participants rows have won=NULL (ignored by recompute).")

        if stats["match_participants_orphaned"] > 0:
            print(
                f"WARNING: {stats['match_participants_orphaned']} match_participants rows reference players not present in players table."
            )
            if args.verbose:
                for discord_id, c in _fetch_orphaned_participants(conn):
                    print(f"  - orphan discord_id={discord_id}: {c} rows")

        print("Validation:")
        print(f"- match_participants total: {stats['match_participants_total']}")
        print(f"- match_participants counted (won in 0/1): {stats['match_participants_counted']}")
        print(f"- players total: {stats['players_total']}")

        changes = _print_report(rows, verbose=args.verbose)
        if changes == 0:
            print("No changes needed.")
            return 0

        if args.dry_run:
            print("Dry-run: no changes written.")
            return 0

    # Apply updates in a separate transaction after backup
    backup_path = _make_backup(db_path)
    print(f"Backup created: {backup_path}")

    with db.connection() as conn:
        rows = _fetch_recalc_rows(conn)
        updated = _apply_updates(conn, rows)
        print(f"Updated {updated} players.")

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))


