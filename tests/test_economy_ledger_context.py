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


def test_miner_respec_records_sink_context_and_action(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    dig_repo = DigRepository(repo_db_path)
    player_repo.add(401, "digger", GUILD_ID)
    player_repo.update_balance(401, GUILD_ID, 100)
    dig_repo.create_tunnel(401, GUILD_ID, "Test Tunnel")
    dig_repo.update_tunnel(
        401,
        GUILD_ID,
        stat_strength=2,
        stat_smarts=1,
        stat_stamina=7,
        stat_points=12,
    )
    _clear_ledger(repo_db_path)

    status = dig_repo.atomic_respec_miner_stats(
        401,
        GUILD_ID,
        cost=50,
    )
    repeated_status = dig_repo.atomic_respec_miner_stats(
        401,
        GUILD_ID,
        cost=50,
    )

    assert status["status"] == "ok"
    assert repeated_status["status"] == "no_allocated_points"
    assert player_repo.get_balance(401, GUILD_ID) == 50
    rows = _ledger_rows(repo_db_path)
    assert len(rows) == 1
    assert rows[0]["delta"] == -50
    assert rows[0]["source"] == "dig"
    assert rows[0]["actor_id"] == 401
    assert rows[0]["related_type"] == "miner_respec"
    assert rows[0]["related_id"] == "s_points"
    assert rows[0]["reason"] == "dig miner respec debit"
    assert json.loads(rows[0]["metadata"]) == {
        "cost": 50,
        "returned_points": 10,
        "previous_stats": {
            "strength": 2,
            "smarts": 1,
            "stamina": 7,
        },
    }

    with sqlite3.connect(repo_db_path) as conn:
        action = conn.execute(
            """
            SELECT action_type, jc_delta, detail
            FROM dig_actions
            WHERE actor_id = ? AND guild_id = ?
            ORDER BY id DESC
            LIMIT 1
            """,
            (401, GUILD_ID),
        ).fetchone()
    assert action[0] == "miner_respec"
    assert action[1] == -50
    assert json.loads(action[2])["returned_points"] == 10
