"""
update_trivia_db.py — Sync hero stats in dotabase.db from local Dota 2 game files.

Run this after a Dota 2 patch to keep trivia questions accurate.
The bot must be offline when you run this; restart it afterward.

Usage:
    uv run python update_trivia_db.py
    uv run python update_trivia_db.py --dota-path "D:/Steam/steamapps/common/dota 2 beta"
    uv run python update_trivia_db.py --dry-run
"""

from __future__ import annotations

import argparse
import re
import sqlite3
from pathlib import Path

import vpk

DEFAULT_DOTA_PATH = "C:/Program Files (x86)/Steam/steamapps/common/dota 2 beta"
NPC_HEROES_PATH = "scripts/npc/npc_heroes.txt"

# Mapping from npc_heroes.txt KV key → dotabase heroes table column
_STAT_MAP: dict[str, tuple[str, type]] = {
    "AttributeAgilityGain":        ("attr_agility_gain",        float),
    "AttributeStrengthGain":       ("attr_strength_gain",       float),
    "AttributeIntelligenceGain":   ("attr_intelligence_gain",   float),
    "AttributeBaseAgility":        ("attr_agility_base",        int),
    "AttributeBaseStrength":       ("attr_strength_base",       int),
    "AttributeBaseIntelligence":   ("attr_intelligence_base",   int),
    "ArmorPhysical":               ("base_armor",               int),
    "MovementSpeed":               ("base_movement",            int),
    "AttackRate":                  ("attack_rate",              float),
}

_HERO_BLOCK_RE = re.compile(r'"(npc_dota_hero_[^"]+)"\s*\{')
_KV_RE = re.compile(r'"([^"]+)"\s+"([^"]+)"')


def parse_hero_stats(vdf_text: str) -> dict[str, dict[str, int | float]]:
    """Parse npc_heroes.txt and return {hero_name: {db_col: value}}."""
    heroes: dict[str, dict[str, int | float]] = {}

    for match in _HERO_BLOCK_RE.finditer(vdf_text):
        name = match.group(1)
        start = match.end()

        # Walk forward to find the matching closing brace
        depth = 0
        end = start
        for i, ch in enumerate(vdf_text[start:], start):
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth < 0:
                    end = i
                    break

        block = vdf_text[start:end]
        stats: dict[str, int | float] = {}
        for kv in _KV_RE.finditer(block):
            key, raw_val = kv.group(1), kv.group(2)
            if key in _STAT_MAP:
                col, cast = _STAT_MAP[key]
                try:
                    stats[col] = cast(raw_val)
                except (ValueError, TypeError):
                    pass
        if stats:
            heroes[name] = stats

    return heroes


def update_dotabase(
    hero_stats: dict[str, dict[str, int | float]],
    *,
    dry_run: bool = False,
) -> tuple[int, int]:
    """
    Apply parsed stats to dotabase.db.

    Returns (heroes_updated, rows_updated).
    """
    import dotabase  # local import so script works even if dotabase import is slow

    db_path = Path(dotabase.__file__).parent / "dotabase.db"
    if not db_path.exists():
        raise FileNotFoundError(f"dotabase.db not found at {db_path}")

    conn = sqlite3.connect(str(db_path))
    cur = conn.cursor()

    heroes_updated = 0
    rows_updated = 0
    changes: list[tuple[str, str, object, object]] = []  # (hero, col, old, new)

    for hero_name, stats in hero_stats.items():
        hero_changed = False
        for col, new_val in stats.items():
            # Read current value for diff display
            cur.execute(f"SELECT {col} FROM heroes WHERE name = ?", (hero_name,))
            row = cur.fetchone()
            if row is None:
                continue  # hero not in dotabase (e.g. disabled heroes)
            old_val = row[0]
            if old_val != new_val:
                changes.append((hero_name, col, old_val, new_val))
                if not dry_run:
                    cur.execute(
                        f"UPDATE heroes SET {col} = ? WHERE name = ?",
                        (new_val, hero_name),
                    )
                rows_updated += 1
                hero_changed = True
        if hero_changed:
            heroes_updated += 1

    if not dry_run:
        conn.commit()
    conn.close()

    return heroes_updated, rows_updated, changes


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Sync dotabase hero stats from local Dota 2 game files."
    )
    parser.add_argument(
        "--dota-path",
        default=DEFAULT_DOTA_PATH,
        help=f"Path to Dota 2 installation (default: {DEFAULT_DOTA_PATH})",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print changes without writing to dotabase.db",
    )
    args = parser.parse_args()

    vpk_path = Path(args.dota_path) / "game/dota/pak01_dir.vpk"
    if not vpk_path.exists():
        print(f"ERROR: VPK not found at {vpk_path}")
        print("Is Dota 2 installed? Use --dota-path to specify the installation directory.")
        raise SystemExit(1)

    print(f"Reading VPK: {vpk_path}")
    pak = vpk.open(str(vpk_path))

    if NPC_HEROES_PATH not in pak:
        print(f"ERROR: {NPC_HEROES_PATH} not found inside VPK")
        raise SystemExit(1)

    raw = pak[NPC_HEROES_PATH].read().decode("utf-8", errors="replace")
    hero_stats = parse_hero_stats(raw)
    print(f"Parsed stats for {len(hero_stats)} heroes")

    heroes_updated, rows_updated, changes = update_dotabase(
        hero_stats, dry_run=args.dry_run
    )

    if changes:
        print(f"\n{'DRY RUN — ' if args.dry_run else ''}Changes:")
        for hero, col, old, new in sorted(changes):
            short = hero.replace("npc_dota_hero_", "")
            print(f"  {short:30s}  {col:28s}  {old!s:>8} → {new!s}")
    else:
        print("No stat differences found — dotabase is already up to date.")

    if not args.dry_run:
        print(
            f"\nUpdated {rows_updated} stat values across {heroes_updated} heroes."
        )
        print("Restart the bot to apply changes (LRU cache must be cleared).")
    else:
        print(f"\n{rows_updated} stat values would be updated across {heroes_updated} heroes.")


if __name__ == "__main__":
    main()
