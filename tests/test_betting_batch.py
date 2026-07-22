"""Regression tests for batched automatic bet placement."""

import json
import sqlite3

import pytest

from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from tests.conftest import TEST_GUILD_ID


def _seed_players(
    player_repo: PlayerRepository,
    balances: dict[int, int],
    *,
    guild_id: int = TEST_GUILD_ID,
) -> None:
    for discord_id, balance in balances.items():
        player_repo.add(
            discord_id=discord_id,
            discord_username=f"Player{discord_id}",
            guild_id=guild_id,
        )
        player_repo.update_balance(discord_id, guild_id, balance)


def _bet_kwargs(
    discord_id: int,
    team: str,
    *,
    pending_match_id: int,
    amount: int = 10,
) -> dict:
    return {
        "guild_id": TEST_GUILD_ID,
        "discord_id": discord_id,
        "team": team,
        "amount": amount,
        "bet_time": 1_700_000_000,
        "since_ts": 1_700_000_000,
        "pending_match_id": pending_match_id,
    }


def test_automatic_bet_batch_uses_one_connection_and_isolates_rejections(
    repo_db_path, monkeypatch
):
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    _seed_players(player_repo, {101: 100, 102: 100})

    connection_count = 0
    original_get_connection = bet_repo.get_connection

    def counted_get_connection():
        nonlocal connection_count
        connection_count += 1
        return original_get_connection()

    monkeypatch.setattr(bet_repo, "get_connection", counted_get_connection)

    with bet_repo.automatic_bet_batch() as place_bet:
        place_bet(**_bet_kwargs(101, "radiant", pending_match_id=11))
        # Pending matches are separate betting windows, so the opposite side
        # is valid for the same player in a different pending match.
        place_bet(
            **_bet_kwargs(101, "dire", pending_match_id=22, amount=5)
        )
        # The same side rule still applies within one pending match. Its
        # savepoint rejection must not discard either surrounding success.
        with pytest.raises(ValueError, match="already have bets on Radiant"):
            place_bet(
                **_bet_kwargs(101, "dire", pending_match_id=11, amount=7)
            )
        place_bet(
            **_bet_kwargs(102, "radiant", pending_match_id=11, amount=8)
        )

    assert connection_count == 1

    with sqlite3.connect(repo_db_path) as conn:
        conn.row_factory = sqlite3.Row
        bets = conn.execute(
            """
            SELECT discord_id, team_bet_on, amount, pending_match_id
            FROM bets
            ORDER BY bet_id
            """
        ).fetchall()
        balances = {
            row["discord_id"]: row["jopacoin_balance"]
            for row in conn.execute(
                "SELECT discord_id, jopacoin_balance FROM players"
            ).fetchall()
        }
        ledger = conn.execute(
            """
            SELECT account_id, delta, source, related_type, related_id, metadata
            FROM economy_ledger_entries
            WHERE source = 'bet'
            ORDER BY ledger_id
            """
        ).fetchall()
        context_count = conn.execute(
            "SELECT COUNT(*) FROM economy_ledger_context"
        ).fetchone()[0]

    assert [tuple(row) for row in bets] == [
        (101, "radiant", 10, 11),
        (101, "dire", 5, 22),
        (102, "radiant", 8, 11),
    ]
    assert balances == {101: 85, 102: 92}
    assert [(row["account_id"], row["delta"]) for row in ledger] == [
        (101, -10),
        (101, -5),
        (102, -8),
    ]
    assert all(row["source"] == "bet" for row in ledger)
    assert all(row["related_type"] == "pending_match" for row in ledger)
    assert [row["related_id"] for row in ledger] == ["11", "22", "11"]
    assert [json.loads(row["metadata"])["team"] for row in ledger] == [
        "radiant",
        "dire",
        "radiant",
    ]
    assert context_count == 0


def test_auto_blinds_batch_connections_partial_failure_and_sequential_odds(
    repo_db_path, monkeypatch
):
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    service = BettingService(bet_repo, player_repo)
    radiant_ids = [201, 202, 203]
    dire_ids = [204, 205]
    _seed_players(
        player_repo,
        {201: 100, 202: -1, 203: 100, 204: 100, 205: 100},
    )

    # Reproduce a stale sizing snapshot: player 202 was eligible when balances
    # were read, but is in debt by the time the write transaction validates it.
    monkeypatch.setattr(
        player_repo,
        "get_balances_bulk",
        lambda discord_ids, guild_id: dict.fromkeys(discord_ids, 100),
    )
    connection_count = 0
    original_get_connection = bet_repo.get_connection

    def counted_get_connection():
        nonlocal connection_count
        connection_count += 1
        return original_get_connection()

    monkeypatch.setattr(bet_repo, "get_connection", counted_get_connection)

    timestamp = 1_700_000_100
    result = service.create_auto_blind_bets(
        guild_id=TEST_GUILD_ID,
        radiant_ids=radiant_ids,
        dire_ids=dire_ids,
        shuffle_timestamp=timestamp,
    )

    # One read connection for the initial pool totals and one shared write
    # connection for every attempted placement.
    assert connection_count == 2
    assert result["created"] == 4
    assert result["total_radiant"] == 20
    assert result["total_dire"] == 20
    assert [bet["discord_id"] for bet in result["bets"]] == [201, 203, 204, 205]
    assert result["skipped"] == [{
        "discord_id": 202,
        "reason": "You cannot place bets while in debt. Win some games to pay it off!",
    }]

    with sqlite3.connect(repo_db_path) as conn:
        conn.row_factory = sqlite3.Row
        bets = conn.execute(
            """
            SELECT discord_id, team_bet_on, amount, odds_at_placement
            FROM bets
            ORDER BY bet_id
            """
        ).fetchall()
        balances = {
            row["discord_id"]: row["jopacoin_balance"]
            for row in conn.execute(
                "SELECT discord_id, jopacoin_balance FROM players"
            ).fetchall()
        }
        bet_ledger_count = conn.execute(
            "SELECT COUNT(*) FROM economy_ledger_entries WHERE source = 'bet'"
        ).fetchone()[0]

    assert [
        (
            row["discord_id"],
            row["team_bet_on"],
            row["amount"],
            row["odds_at_placement"],
        )
        for row in bets
    ] == [
        (201, "radiant", 10, None),
        (203, "radiant", 10, 1.0),
        (204, "dire", 10, None),
        (205, "dire", 10, 3.0),
    ]
    assert balances == {201: 90, 202: -1, 203: 90, 204: 90, 205: 90}
    assert bet_ledger_count == 4


def test_auto_spectators_share_transaction_and_ignore_failed_candidate_in_odds(
    repo_db_path, monkeypatch
):
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    service = BettingService(bet_repo, player_repo)
    spectator_snapshots = [
        {"discord_id": 301, "jopacoin_balance": 500},
        {"discord_id": 302, "jopacoin_balance": 400},
        {"discord_id": 303, "jopacoin_balance": 300},
        {"discord_id": 304, "jopacoin_balance": 200},
        {"discord_id": 305, "jopacoin_balance": 100},
    ]
    _seed_players(
        player_repo,
        {301: 500, 302: -1, 303: 300, 304: 200, 305: 100},
    )
    monkeypatch.setattr(
        player_repo,
        "get_richest_players",
        lambda guild_id, limit, min_balance: spectator_snapshots,
    )
    connection_count = 0
    original_get_connection = bet_repo.get_connection

    def counted_get_connection():
        nonlocal connection_count
        connection_count += 1
        return original_get_connection()

    monkeypatch.setattr(bet_repo, "get_connection", counted_get_connection)

    timestamp = 1_700_000_200
    result = service.create_auto_spectator_bets(
        guild_id=TEST_GUILD_ID,
        radiant_ids=list(range(401, 406)),
        dire_ids=list(range(406, 411)),
        shuffle_timestamp=timestamp,
    )

    assert connection_count == 2
    assert result["created"] == 4
    assert [bet["discord_id"] for bet in result["bets"]] == [301, 303, 304, 305]
    assert result["skipped"] == [{
        "discord_id": 302,
        "reason": "You cannot place bets while in debt. Win some games to pay it off!",
    }]

    with sqlite3.connect(repo_db_path) as conn:
        conn.row_factory = sqlite3.Row
        rows_by_id = {
            row["discord_id"]: row
            for row in conn.execute(
                """
                SELECT discord_id, team_bet_on, amount, odds_at_placement
                FROM bets
                ORDER BY bet_id
                """
            ).fetchall()
        }

    running_totals = {"radiant": 0, "dire": 0}
    for bet in result["bets"]:
        row = rows_by_id[bet["discord_id"]]
        team = bet["team"]
        total_pool = running_totals["radiant"] + running_totals["dire"]
        team_total = running_totals[team]
        expected_odds = total_pool / team_total if team_total > 0 else None
        assert row["team_bet_on"] == team
        assert row["amount"] == bet["amount"]
        assert row["odds_at_placement"] == expected_odds
        running_totals[team] += bet["amount"]

    assert set(rows_by_id) == {301, 303, 304, 305}
    assert running_totals == {
        "radiant": result["total_radiant"],
        "dire": result["total_dire"],
    }
