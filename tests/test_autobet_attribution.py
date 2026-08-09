"""Schema and repository contracts for automatic-bet attribution."""

import pytest

from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID


def _register_investor(
    repo_db_path: str, *, discord_id: int = 101
) -> tuple[PlayerRepository, BetRepository]:
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, 100)
    return player_repo, bet_repo


def test_autobet_attribution_migration_adds_columns_and_profile_index(repo_db_path):
    bet_repo = BetRepository(repo_db_path)

    with bet_repo.connection() as conn:
        columns = {row["name"]: row for row in conn.execute("PRAGMA table_info(bets)").fetchall()}
        indexes = {row["name"] for row in conn.execute("PRAGMA index_list(bets)").fetchall()}
        index_columns = [
            row["name"]
            for row in conn.execute("PRAGMA index_info(idx_bets_autobet_profile)").fetchall()
        ]
        migration = conn.execute(
            "SELECT 1 FROM schema_migrations WHERE name = ?",
            ("add_autobet_attribution_to_bets",),
        ).fetchone()

    assert columns["investment_target_id"]["type"] == "INTEGER"
    assert columns["investment_target_id"]["notnull"] == 0
    assert columns["investment_direction"]["type"] == "TEXT"
    assert columns["investment_direction"]["notnull"] == 0
    assert "idx_bets_autobet_profile" in indexes
    assert index_columns == [
        "guild_id",
        "discord_id",
        "match_id",
        "is_blind",
        "investment_target_id",
    ]
    assert migration is not None


def test_repository_defaults_generic_auto_and_persists_player_attribution(repo_db_path):
    _player_repo, bet_repo = _register_investor(repo_db_path)
    since_ts = 1_700_000_000

    generic_id = bet_repo.place_bet_atomic(
        guild_id=TEST_GUILD_ID,
        discord_id=101,
        team="radiant",
        amount=10,
        bet_time=since_ts,
        since_ts=since_ts,
        is_blind=True,
    )
    investment_id = bet_repo.place_bet_atomic(
        guild_id=TEST_GUILD_ID,
        discord_id=101,
        team="radiant",
        amount=7,
        bet_time=since_ts + 1,
        since_ts=since_ts,
        is_blind=True,
        investment_target_id=202,
        investment_direction="short",
    )

    with bet_repo.connection() as conn:
        rows = conn.execute(
            """
            SELECT bet_id, is_blind, investment_target_id, investment_direction
            FROM bets
            WHERE bet_id IN (?, ?)
            ORDER BY bet_id
            """,
            (generic_id, investment_id),
        ).fetchall()

    assert [dict(row) for row in rows] == [
        {
            "bet_id": generic_id,
            "is_blind": 1,
            "investment_target_id": None,
            "investment_direction": None,
        },
        {
            "bet_id": investment_id,
            "is_blind": 1,
            "investment_target_id": 202,
            "investment_direction": "short",
        },
    ]


@pytest.mark.parametrize(
    ("is_blind", "target_id", "direction", "message"),
    [
        (True, None, "long", "direction requires a target"),
        (False, 202, "long", "requires an automatic bet"),
        (True, 202, None, "must be long or short"),
        (True, 202, "sideways", "must be long or short"),
    ],
)
def test_repository_rejects_inconsistent_attribution_before_debit(
    repo_db_path,
    is_blind,
    target_id,
    direction,
    message,
):
    player_repo, bet_repo = _register_investor(repo_db_path)

    with pytest.raises(ValueError, match=message):
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=101,
            team="radiant",
            amount=10,
            bet_time=1_700_000_000,
            since_ts=1_700_000_000,
            is_blind=is_blind,
            investment_target_id=target_id,
            investment_direction=direction,
        )

    assert player_repo.get_balance(101, TEST_GUILD_ID) == 100
    assert (
        bet_repo.get_bets_for_pending_match(
            TEST_GUILD_ID,
            since_ts=1_700_000_000,
        )
        == []
    )
