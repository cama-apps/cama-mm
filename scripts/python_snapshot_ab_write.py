#!/usr/bin/env python3
"""Exercise the retained Python repository writes for the snapshot A/B gate.

The caller supplies a disposable, already-migrated database copy.  The
fixture-only player/tunnel rows are seeded with SQLite because the production
repositories intentionally do not create account records.  Every transition
under comparison then goes through the retained production repositories:
guild configuration, Dig inventory/route state, and Survey delivery.

This process exits before the Rust-side copy is touched.  That ordering is a
deliberate part of the A/B evidence, not an attempt to run both runtimes in
one process.
"""

from __future__ import annotations

import sqlite3
import sys
import time
from pathlib import Path

# Running a helper by path puts ``scripts/`` on sys.path, not the project root.
PROJECT_ROOT = str(Path(__file__).resolve().parents[1])
if PROJECT_ROOT not in sys.path:
    sys.path.insert(0, PROJECT_ROOT)

from repositories.dig_repository import DigRepository
from repositories.guild_config_repository import GuildConfigRepository
from repositories.survey_repository import SurveyRepository

SMOKE_GUILD_ID = -8_888_888_888_888_888_888
DIG_PLAYER_ID = -8_888_888_888_888_888_870
SURVEY_GUILD_ID = 9_000_000_000_000_001
SURVEY_USER_ID = 9_000_000_000_000_002
SURVEY_TITLE = "Rust disposable Survey recovery smoke"
SURVEY_PROMPT = "Can the Rust recovery worker deliver this clone-only sentinel?"
SURVEY_CHANNEL_ID = 8_000_000_000_000_001
SURVEY_MESSAGE_ID = 8_000_000_000_000_002


def _seed_dig_fixture(db_path: str) -> None:
    """Create only the account rows required by the repository APIs.

    This setup is not part of the transition contract.  It is the same
    production-shaped prerequisite that the Rust repository smoke creates
    before buying an inventory item and updating a tunnel route.
    """

    connection = sqlite3.connect(db_path)
    try:
        connection.execute("PRAGMA foreign_keys = OFF")
        if connection.execute(
            "SELECT 1 FROM players WHERE discord_id = ? AND guild_id = ?",
            (DIG_PLAYER_ID, SMOKE_GUILD_ID),
        ).fetchone():
            raise RuntimeError("Dig A/B player sentinel already exists")
        if connection.execute(
            "SELECT 1 FROM tunnels WHERE discord_id = ? AND guild_id = ?",
            (DIG_PLAYER_ID, SMOKE_GUILD_ID),
        ).fetchone():
            raise RuntimeError("Dig A/B tunnel sentinel already exists")
        connection.execute(
            """
            INSERT INTO players (
                discord_id, guild_id, discord_username, jopacoin_balance,
                glicko_rating, glicko_rd, glicko_volatility
            ) VALUES (?, ?, ?, ?, NULL, NULL, NULL)
            """,
            (DIG_PLAYER_ID, SMOKE_GUILD_ID, "Python A/B inventory miner", 100),
        )
        connection.execute(
            "INSERT INTO tunnels (discord_id, guild_id, depth) VALUES (?, ?, 100)",
            (DIG_PLAYER_ID, SMOKE_GUILD_ID),
        )
        connection.commit()
    finally:
        connection.close()


def write_snapshot(db_path: str) -> None:
    """Apply the bounded Python-side transition and verify its readback."""

    _seed_dig_fixture(db_path)

    guild_repository = GuildConfigRepository(db_path)
    guild_repository.set_league_id(SMOKE_GUILD_ID, 777)
    guild_repository.set_auto_enrich(SMOKE_GUILD_ID, False)
    guild_repository.set_ai_enabled(SMOKE_GUILD_ID, True)
    guild = guild_repository.get_config(SMOKE_GUILD_ID)
    if guild is None or (
        guild["league_id"],
        guild["auto_enrich_matches"],
        guild["ai_features_enabled"],
    ) != (777, 0, 1):
        raise RuntimeError(f"Python guild-config A/B write mismatch: {guild!r}")

    dig_repository = DigRepository(db_path)
    purchased = dig_repository.atomic_auto_buy_items(
        DIG_PLAYER_ID,
        SMOKE_GUILD_ID,
        queue_item_ids=[],
        purchases=[("hard_hat", 20)],
    )
    if len(purchased) != 1 or purchased[0] is None:
        raise RuntimeError(f"Python Dig inventory A/B write mismatch: {purchased!r}")
    route_state = '{"route_id":"shored_passage","status":"active"}'
    dig_repository.update_tunnel(
        DIG_PLAYER_ID,
        SMOKE_GUILD_ID,
        route_state=route_state,
    )
    inventory = dig_repository.get_inventory(DIG_PLAYER_ID, SMOKE_GUILD_ID)
    tunnel = dig_repository.get_tunnel(DIG_PLAYER_ID, SMOKE_GUILD_ID)
    if len(inventory) != 1 or inventory[0]["item_type"] != "hard_hat" or not inventory[0]["queued"]:
        raise RuntimeError(f"Python Dig inventory readback mismatch: {inventory!r}")
    if tunnel is None or tunnel["depth"] != 100:
        raise RuntimeError(f"Python Dig tunnel readback mismatch: {tunnel!r}")

    survey_repository = SurveyRepository(db_path)
    if any(
        survey.title == SURVEY_TITLE
        for survey in survey_repository.list_surveys(SURVEY_GUILD_ID)
    ):
        raise RuntimeError("Python Survey A/B sentinel already exists")
    survey = survey_repository.create_survey(SURVEY_GUILD_ID, SURVEY_TITLE)
    question = survey_repository.add_question(
        SURVEY_GUILD_ID,
        survey.survey_id,
        SURVEY_PROMPT,
        "nps",
        True,
    )
    survey_repository.open_survey(
        SURVEY_GUILD_ID,
        survey.survey_id,
        [SURVEY_USER_ID],
        target_type="member",
        registered_only=False,
    )
    claimed = survey_repository.claim_deliveries(limit=1, stale_after_seconds=300)
    if len(claimed) != 1 or claimed[0].survey_id != survey.survey_id:
        raise RuntimeError(f"Python Survey claim A/B write mismatch: {claimed!r}")
    sent = survey_repository.mark_delivery_sent(
        claimed[0].recipient_id,
        claimed[0].attempt_count,
        SURVEY_CHANNEL_ID,
        SURVEY_MESSAGE_ID,
    )
    if (
        sent.delivery_status.value != "sent"
        or sent.attempt_count != 1
        or sent.dm_channel_id != SURVEY_CHANNEL_ID
        or sent.dm_message_id != SURVEY_MESSAGE_ID
        or sent.current_question_id != question.question_id
    ):
        raise RuntimeError(f"Python Survey delivery A/B write mismatch: {sent!r}")

    print(
        "python_snapshot_ab_write=ok "
        f"guild_id={SMOKE_GUILD_ID} dig_item=hard_hat route=shored_passage "
        f"survey_id={survey.survey_id} delivery=sent at={int(time.time())}"
    )


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: python_snapshot_ab_write.py <disposable-db>", file=sys.stderr)
        return 2
    try:
        write_snapshot(sys.argv[1])
    except (OSError, RuntimeError, sqlite3.Error, ValueError) as error:
        print(f"Python snapshot A/B write failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
