import json
import sqlite3

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository

GUILD_ID = 12345


def _clear_ledger(db_path: str) -> None:
    with sqlite3.connect(db_path) as conn:
        conn.execute("DELETE FROM economy_ledger_entries")


def _ledger_rows(db_path: str) -> list[dict]:
    with sqlite3.connect(db_path) as conn:
        conn.row_factory = sqlite3.Row
        rows = conn.execute(
            """
            SELECT account_type, account_id, delta, balance_after, source,
                   actor_id, related_type, related_id, reason, metadata
            FROM economy_ledger_entries
            ORDER BY ledger_id
            """
        ).fetchall()
    return [dict(row) for row in rows]


def test_player_balance_context_is_recorded_and_does_not_leak(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(111, "player", GUILD_ID)
    _clear_ledger(repo_db_path)

    player_repo.add_balance(
        111,
        GUILD_ID,
        10,
        source="gamba",
        actor_id=111,
        related_type="wheel_spin",
        related_id="LIGHTNING_BOLT",
        reason="gamba lightning bolt tax",
        metadata={"tax_pct": 0.02},
    )
    player_repo.add_balance(111, GUILD_ID, 1)

    rows = _ledger_rows(repo_db_path)
    assert rows[0]["source"] == "gamba"
    assert rows[0]["actor_id"] == 111
    assert rows[0]["related_type"] == "wheel_spin"
    assert rows[0]["related_id"] == "LIGHTNING_BOLT"
    assert rows[0]["reason"] == "gamba lightning bolt tax"
    assert json.loads(rows[0]["metadata"]) == {"tax_pct": 0.02}

    assert rows[1]["source"] == "balance_update"
    assert rows[1]["reason"] is None


def test_gamba_steal_context_records_victim_and_thief_rows(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(201, "spinner", GUILD_ID)
    player_repo.add(202, "victim", GUILD_ID)
    player_repo.update_balance(201, GUILD_ID, 10)
    player_repo.update_balance(202, GUILD_ID, 30)
    _clear_ledger(repo_db_path)

    player_repo.steal_atomic(
        thief_discord_id=201,
        victim_discord_id=202,
        guild_id=GUILD_ID,
        amount=7,
        source="gamba",
        actor_id=201,
        related_type="wheel_spin",
        related_id="GREEN_SHELL",
        reason="gamba green shell steal",
    )

    rows = _ledger_rows(repo_db_path)
    assert [(row["account_id"], row["delta"], row["source"]) for row in rows] == [
        (202, -7, "gamba"),
        (201, 7, "gamba"),
    ]
    assert rows[0]["reason"] == "gamba green shell steal victim debit"
    assert rows[1]["reason"] == "gamba green shell steal thief credit"


def test_dig_atomic_balance_update_records_dig_context(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    dig_repo = DigRepository(repo_db_path)
    player_repo.add(301, "digger", GUILD_ID)
    dig_repo.create_tunnel(301, GUILD_ID, "Test Tunnel")
    _clear_ledger(repo_db_path)

    dig_repo.atomic_tunnel_balance_update(
        301,
        GUILD_ID,
        balance_delta=5,
        tunnel_updates={"depth": 3},
        log_detail={"event_id": "crystal_garden", "choice": "safe"},
        log_action_type="event",
    )

    rows = _ledger_rows(repo_db_path)
    assert len(rows) == 1
    assert rows[0]["source"] == "dig"
    assert rows[0]["actor_id"] == 301
    assert rows[0]["related_type"] == "event"
    assert rows[0]["related_id"] == "crystal_garden"
    assert rows[0]["reason"] == "dig event credit"
    assert json.loads(rows[0]["metadata"]) == {
        "event_id": "crystal_garden",
        "choice": "safe",
    }
