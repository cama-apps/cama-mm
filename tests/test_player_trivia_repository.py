"""Persistence tests for guild-scoped player-history trivia."""

from __future__ import annotations

import json
import threading
from concurrent.futures import ThreadPoolExecutor

import pytest

from repositories.player_trivia_repository import PlayerTriviaRepository
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


@pytest.fixture
def trivia_repo(repo_db_path):
    return PlayerTriviaRepository(repo_db_path)


def _questions() -> list[dict]:
    return [
        {
            "question_key": "economy:balance:alpha",
            "category": "economy",
            "spicy": False,
            "question_text": "Who has the largest balance?",
            "options": ["Alpha", "Bravo", "Charlie", "Delta"],
            "correct_index": 1,
            "explanation": "Bravo has the largest stored balance.",
        },
        {
            "question_key": "matches:hero:alpha",
            "category": "heroes",
            "spicy": True,
            "question_text": "Which hero did Alpha play most recently?",
            "options": ["Axe", "Bane", "Chen", "Doom"],
            "correct_index": 2,
            "explanation": "Chen was Alpha's most recent hero.",
        },
    ]


def _add_player(player_repository, discord_id: int, guild_id: int, name: str) -> None:
    player_repository.add(discord_id, name, guild_id)


def test_schema_contains_player_trivia_and_history_extensions(trivia_repo):
    with trivia_repo.connection() as conn:
        tables = {
            row["name"]
            for row in conn.execute("SELECT name FROM sqlite_master WHERE type = 'table'")
        }
        wheel_columns = {row["name"] for row in conn.execute("PRAGMA table_info(wheel_spins)")}
        indexes = {
            row["name"]
            for row in conn.execute("SELECT name FROM sqlite_master WHERE type = 'index'")
        }

    assert "player_trivia_sessions" in tables
    assert "player_trivia_questions" in tables
    assert "disburse_vote_history" in tables
    assert {"outcome_code", "is_bonus", "event_id", "outcome_metadata"} <= wheel_columns
    assert "idx_player_trivia_sessions_guild_user_start" in indexes
    assert "idx_wheel_spins_guild_player_time" in indexes
    assert "idx_disburse_vote_history_voter" in indexes


def test_start_persists_an_immutable_snapshot_and_enforces_cooldown(trivia_repo, player_repository):
    _add_player(player_repository, 101, TEST_GUILD_ID, "Alpha")
    session_id = trivia_repo.try_start_session(
        101,
        TEST_GUILD_ID,
        _questions(),
        now=10_000,
        cooldown_seconds=3_600,
    )

    assert isinstance(session_id, int)
    assert trivia_repo.get_session(session_id) == {
        "session_id": session_id,
        "guild_id": TEST_GUILD_ID,
        "discord_id": 101,
        "started_at": 10_000,
        "completed_at": None,
        "status": "active",
        "question_count": 2,
        "score": 0,
        "jc_earned": 0,
    }
    stored = trivia_repo.get_questions(session_id)
    assert [row["question_key"] for row in stored] == [
        "economy:balance:alpha",
        "matches:hero:alpha",
    ]
    assert stored[0]["options"] == ["Alpha", "Bravo", "Charlie", "Delta"]
    assert stored[0]["correct_index"] == 1
    assert stored[1]["spicy"] is True

    # Mutating generation output cannot alter the already persisted game.
    source = _questions()
    second_id = trivia_repo.try_start_session(
        101,
        TEST_GUILD_ID,
        source,
        now=10_001,
        cooldown_seconds=3_600,
    )
    source[0]["options"][1] = "Mutated"
    assert second_id is None
    assert trivia_repo.get_questions(session_id)[0]["options"][1] == "Bravo"

    assert trivia_repo.get_last_session_started(101, TEST_GUILD_ID) == 10_000
    assert trivia_repo.get_recent_question_keys(101, TEST_GUILD_ID, 9_999) == {
        "economy:balance:alpha",
        "matches:hero:alpha",
    }
    assert trivia_repo.get_recent_question_keys(101, TEST_GUILD_ID, 10_001) == set()

    bypassed_id = trivia_repo.try_start_session(
        101,
        TEST_GUILD_ID,
        _questions(),
        now=10_002,
        cooldown_seconds=3_600,
        bypass=True,
    )
    assert isinstance(bypassed_id, int)


@pytest.mark.parametrize(
    "options",
    [
        ["A", "B", "C"],
        ["A", "B", "C", "D", "E"],
        ["A", "B", "C", "a"],
    ],
)
def test_start_requires_exactly_four_distinct_options(trivia_repo, options):
    question = _questions()[0]
    question["options"] = options

    with pytest.raises(ValueError):
        trivia_repo.try_start_session(
            101,
            TEST_GUILD_ID,
            [question],
            now=10_000,
            cooldown_seconds=0,
        )


def test_cancelled_unanswered_session_releases_cooldown(
    trivia_repo, player_repository, monkeypatch
):
    _add_player(player_repository, 102, TEST_GUILD_ID, "Bravo")
    session_id = trivia_repo.try_start_session(
        102,
        TEST_GUILD_ID,
        _questions(),
        now=20_000,
        cooldown_seconds=3_600,
    )
    monkeypatch.setattr("repositories.player_trivia_repository.time.time", lambda: 20_001)

    assert trivia_repo.cancel_session_if_unanswered(session_id) is True
    assert trivia_repo.cancel_session_if_unanswered(session_id) is False
    assert trivia_repo.get_session(session_id)["status"] == "cancelled"
    assert trivia_repo.get_session(session_id)["completed_at"] == 20_001
    assert trivia_repo.get_last_session_started(102, TEST_GUILD_ID) is None
    assert trivia_repo.get_recent_question_keys(102, TEST_GUILD_ID, 0) == set()

    replacement = trivia_repo.try_start_session(
        102,
        TEST_GUILD_ID,
        _questions(),
        now=20_002,
        cooldown_seconds=3_600,
    )
    assert isinstance(replacement, int)


def test_settle_answer_is_ordered_idempotent_and_ledger_backed(trivia_repo, player_repository):
    _add_player(player_repository, 103, TEST_GUILD_ID, "Charlie")
    session_id = trivia_repo.try_start_session(
        103,
        TEST_GUILD_ID,
        _questions(),
        now=30_000,
        cooldown_seconds=3_600,
    )

    # A forged/stale component cannot skip the first unanswered round.
    assert trivia_repo.settle_answer(session_id, 2, 2, 7, 30_001) is None
    assert trivia_repo.get_questions(session_id)[1]["selected_index"] is None

    first = trivia_repo.settle_answer(session_id, 1, 1, 7, 30_002)
    assert first == {
        "accepted": True,
        "is_correct": True,
        "reward": 7,
        "score": 1,
        "jc_earned": 7,
        "complete": False,
        "completed": False,
        "new_balance": 10,
    }
    assert trivia_repo.settle_answer(session_id, 1, 1, 7, 30_003) is None

    final = trivia_repo.settle_answer(session_id, 2, 0, 99, 30_004)
    assert final == {
        "accepted": True,
        "is_correct": False,
        "reward": 0,
        "score": 1,
        "jc_earned": 7,
        "complete": True,
        "completed": True,
        "new_balance": 10,
    }
    assert trivia_repo.settle_answer(session_id, 2, 2, 99, 30_005) is None

    session = trivia_repo.get_session(session_id)
    assert session["status"] == "completed"
    assert session["completed_at"] == 30_004
    assert session["score"] == 1
    assert session["jc_earned"] == 7
    questions = trivia_repo.get_questions(session_id)
    assert questions[0]["is_correct"] is True
    assert questions[0]["reward"] == 7
    assert questions[1]["is_correct"] is False
    assert questions[1]["reward"] == 0

    with trivia_repo.connection() as conn:
        player = conn.execute(
            "SELECT jopacoin_balance FROM players WHERE guild_id = ? AND discord_id = ?",
            (TEST_GUILD_ID, 103),
        ).fetchone()
        entries = conn.execute(
            """
            SELECT guild_id, account_id, delta, balance_before, balance_after,
                   source, actor_id, related_type, related_id, metadata
            FROM economy_ledger_entries
            WHERE source = 'player_trivia'
            """
        ).fetchall()

    assert player["jopacoin_balance"] == 10
    assert len(entries) == 1
    entry = dict(entries[0])
    assert entry["guild_id"] == TEST_GUILD_ID
    assert entry["account_id"] == 103
    assert entry["delta"] == 7
    assert (entry["balance_before"], entry["balance_after"]) == (3, 10)
    assert entry["actor_id"] == 103
    assert entry["related_type"] == "player_trivia_session"
    assert entry["related_id"] == str(session_id)
    metadata = json.loads(entry["metadata"])
    assert metadata == {
        "session_id": session_id,
        "question_number": 1,
        "question_key": "economy:balance:alpha",
        "category": "economy",
        "selected_index": 1,
        "correct_index": 1,
        "reward": 7,
        "answered_at": 30_002,
    }


def test_answered_session_cannot_be_cancelled(trivia_repo, player_repository):
    _add_player(player_repository, 104, TEST_GUILD_ID, "Delta")
    session_id = trivia_repo.try_start_session(
        104, TEST_GUILD_ID, _questions(), now=40_000, cooldown_seconds=3_600
    )
    trivia_repo.settle_answer(session_id, 1, 0, 5, 40_001)

    assert trivia_repo.cancel_session_if_unanswered(session_id, now=40_002) is False
    trivia_repo.finish_session(session_id, "timed_out", now=40_003)
    session = trivia_repo.get_session(session_id)
    assert session["status"] == "timed_out"
    assert session["completed_at"] == 40_003


def test_atomic_cooldown_allows_only_one_concurrent_start(
    trivia_repo, player_repository, repo_db_path
):
    _add_player(player_repository, 105, TEST_GUILD_ID, "Echo")
    barrier = threading.Barrier(2)

    def start() -> int | None:
        contender = PlayerTriviaRepository(repo_db_path)
        barrier.wait(timeout=5)
        return contender.try_start_session(
            105,
            TEST_GUILD_ID,
            _questions(),
            now=50_000,
            cooldown_seconds=3_600,
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        results = list(executor.map(lambda _index: start(), range(2)))

    assert sum(isinstance(result, int) for result in results) == 1
    assert results.count(None) == 1


def _seed_snapshot_guild(trivia_repo, guild_id: int, base_id: int) -> dict[str, int]:
    player1 = base_id + 1
    player2 = base_id + 2
    stamp = base_id * 10
    with trivia_repo.connection() as conn:
        conn.execute(
            """
            INSERT INTO players (
                guild_id, discord_id, discord_username, wins, losses,
                jopacoin_balance, preferred_roles, main_role
            ) VALUES (?, ?, ?, 8, 2, 42, '[\"1\",\"2\"]', '2')
            """,
            (guild_id, player1, f"guild-{guild_id}-one"),
        )
        conn.execute(
            """
            INSERT INTO players (
                guild_id, discord_id, discord_username, wins, losses, jopacoin_balance
            ) VALUES (?, ?, ?, 4, 6, 9)
            """,
            (guild_id, player2, f"guild-{guild_id}-two"),
        )
        cursor = conn.execute(
            """
            INSERT INTO matches (
                guild_id, team1_players, team2_players, winning_team, match_date,
                notes, enrichment_data, lobby_type, betting_mode, balancing_rating_system
            ) VALUES (?, ?, ?, 1, ?, 'private note', 'private enrichment',
                      'shuffle', 'house', 'glicko')
            """,
            (guild_id, json.dumps([player1]), json.dumps([player2]), stamp),
        )
        match_id = int(cursor.lastrowid)
        conn.execute(
            """
            INSERT INTO match_participants (
                guild_id, match_id, discord_id, team_number, won, side, hero_id,
                kills, deaths, assists, gpm, lane_role, fantasy_points, bonus_jc
            ) VALUES (?, ?, ?, 1, 1, 'radiant', 2, 5, 1, 9, 600, 2, 33.5, 4)
            """,
            (guild_id, match_id, player1),
        )
        conn.execute(
            """
            INSERT INTO rating_history (
                guild_id, discord_id, match_id, rating, rating_before, rd_before,
                rd_after, team_number, won, timestamp
            ) VALUES (?, ?, ?, 1510, 1500, 90, 85, 1, 1, ?)
            """,
            (guild_id, player1, match_id, stamp),
        )
        conn.execute(
            """
            INSERT INTO player_pairings (
                guild_id, player1_id, player2_id, games_together, wins_together,
                games_against, player1_wins_against, last_match_id
            ) VALUES (?, ?, ?, 3, 2, 4, 3, ?)
            """,
            (guild_id, player1, player2, match_id),
        )
        conn.execute(
            """
            INSERT INTO bankruptcy_state (
                guild_id, discord_id, last_bankruptcy_at,
                penalty_games_remaining, bankruptcy_count
            ) VALUES (?, ?, ?, 1, 2)
            """,
            (guild_id, player1, stamp + 1),
        )
        conn.executemany(
            """
            INSERT INTO wheel_spins (
                guild_id, discord_id, result, spin_time, is_bankrupt, is_golden,
                outcome_code, is_bonus, event_id, outcome_metadata
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            [
                (
                    guild_id,
                    player1,
                    "later",
                    stamp + 3,
                    0,
                    1,
                    "golden",
                    1,
                    f"event-{guild_id}-2",
                    '{"amount": 20}',
                ),
                (
                    guild_id,
                    player1,
                    "earlier",
                    stamp + 2,
                    1,
                    0,
                    "bankrupt",
                    0,
                    f"event-{guild_id}-1",
                    '{"amount": -42}',
                ),
            ],
        )
        conn.execute(
            """
            INSERT INTO double_or_nothing_spins (
                guild_id, discord_id, cost, balance_before, balance_after, won, spin_time
            ) VALUES (?, ?, 5, 10, 15, 1, ?)
            """,
            (guild_id, player1, stamp + 4),
        )
        conn.execute(
            """
            INSERT INTO tip_transactions (
                guild_id, sender_id, recipient_id, amount, fee, timestamp
            ) VALUES (?, ?, ?, 3, 1, ?)
            """,
            (guild_id, player1, player2, stamp + 5),
        )
        conn.execute(
            """
            INSERT INTO bets (
                guild_id, match_id, discord_id, team_bet_on, amount, leverage,
                bet_time, payout, is_blind, odds_at_placement
            ) VALUES (?, ?, ?, '1', 5, 2, ?, 20, 0, 0.5)
            """,
            (guild_id, match_id, player1, stamp + 6),
        )
        cursor = conn.execute(
            """
            INSERT INTO predictions (
                guild_id, creator_id, question, status, outcome, created_at,
                closes_at, resolved_at, current_price, initial_fair
            ) VALUES (?, ?, 'private market text', 'resolved', 'yes', ?, ?, ?, 80, 50)
            """,
            (guild_id, player1, stamp + 7, stamp + 8, stamp + 9),
        )
        prediction_id = int(cursor.lastrowid)
        conn.execute(
            """
            INSERT INTO prediction_positions (
                prediction_id, discord_id, yes_contracts, yes_cost_basis_total,
                no_contracts, no_cost_basis_total, bankruptcy_penalty
            ) VALUES (?, ?, 2, 100, 0, 0, 1)
            """,
            (prediction_id, player1),
        )
        conn.execute(
            """
            INSERT INTO prediction_trades (
                prediction_id, discord_id, action, contracts, jopacoins,
                vwap_x100, last_fill_price, trade_time
            ) VALUES (?, ?, 'buy_yes', 2, 100, 5000, 55, ?)
            """,
            (prediction_id, player1, stamp + 8),
        )
        conn.execute(
            """
            INSERT INTO tunnels (
                guild_id, discord_id, depth, max_depth, total_digs,
                total_jc_earned, prestige_level, tunnel_name, miner_about,
                stat_strength, stat_smarts, stat_stamina, stat_points
            ) VALUES (?, ?, 12, 50, 90, 200, 3, 'private tunnel',
                      'private profile', 4, 5, 6, 7)
            """,
            (guild_id, player1),
        )
        conn.execute(
            """
            INSERT INTO dig_artifacts (
                guild_id, discord_id, artifact_id, found_at, is_relic, equipped
            ) VALUES (?, ?, 'fossil', ?, 1, 1)
            """,
            (guild_id, player1, stamp + 10),
        )
        conn.execute(
            """
            INSERT INTO dig_achievements (
                guild_id, discord_id, achievement_id, unlocked_at
            ) VALUES (?, ?, 'deep_digger', ?)
            """,
            (guild_id, player1, stamp + 11),
        )
        conn.execute(
            """
            INSERT INTO dig_actions (
                guild_id, actor_id, target_id, action_type, depth_before,
                depth_after, jc_delta, detail, created_at
            ) VALUES (?, ?, ?, 'cheer', 11, 12, 2, 'private detail', ?)
            """,
            (guild_id, player1, player2, stamp + 12),
        )
        cursor = conn.execute(
            """
            INSERT INTO mafia_games (
                guild_id, game_date, phase, started_at, winner, entry_fee,
                payout_per_winner, mvp_id, roster_size, twist_event, day_number, status
            ) VALUES (?, ?, 'RESOLVED', ?, 'TOWN', 3, 7, ?, 8,
                      'double_vote', 2, 'ACTIVE')
            """,
            (guild_id, f"date-{guild_id}", stamp + 13, player1),
        )
        game_id = int(cursor.lastrowid)
        conn.execute(
            """
            INSERT INTO mafia_players (
                guild_id, game_id, discord_id, role, is_godfather, hero_name,
                is_alive, eliminated_phase, eliminated_at, acted
            ) VALUES (?, ?, ?, 'TOWNIE', 0, 'Axe', 1, NULL, NULL, 1)
            """,
            (guild_id, game_id, player1),
        )
        conn.execute(
            """
            INSERT INTO mafia_actions (
                guild_id, game_id, actor_id, target_id, action_type, phase,
                day_number, created_at, result
            ) VALUES (?, ?, ?, ?, 'VOTE', 'DAY', 2, ?, 'private result')
            """,
            (guild_id, game_id, player1, player2, stamp + 14),
        )
        conn.execute(
            """
            INSERT INTO recalibration_state (
                guild_id, discord_id, last_recalibration_at,
                total_recalibrations, rating_at_recalibration
            ) VALUES (?, ?, ?, 2, 1400)
            """,
            (guild_id, player1, stamp + 15),
        )
        conn.execute(
            """
            INSERT INTO trivia_sessions (
                guild_id, discord_id, streak, jc_earned, played_at
            ) VALUES (?, ?, 6, 12, ?)
            """,
            (guild_id, player1, stamp + 16),
        )
        conn.execute(
            """
            INSERT INTO disburse_vote_history (
                guild_id, proposal_id, discord_id, vote_method, voted_at,
                proposal_outcome, finalized_at
            ) VALUES (?, ?, ?, 'button', ?, 'approved', ?)
            """,
            (guild_id, base_id, player1, stamp + 17, stamp + 18),
        )
        conn.execute(
            """
            INSERT INTO protected_hero_purchases (
                guild_id, pending_match_id, match_id, discord_id, team_side,
                hero_id, cost, status, purchased_at, resolved_at
            ) VALUES (?, ?, ?, ?, 'radiant', 2, 4, 'recorded', ?, ?)
            """,
            (guild_id, base_id, match_id, player1, stamp + 19, stamp + 20),
        )

    return {
        "player1": player1,
        "player2": player2,
        "match_id": match_id,
        "prediction_id": prediction_id,
        "game_id": game_id,
    }


def test_load_snapshot_is_complete_deterministic_private_and_guild_scoped(trivia_repo):
    ids = _seed_snapshot_guild(trivia_repo, TEST_GUILD_ID, 1_000)
    other = _seed_snapshot_guild(trivia_repo, TEST_GUILD_ID_SECONDARY, 2_000)

    snapshot = trivia_repo.load_snapshot(TEST_GUILD_ID)

    assert list(snapshot) == [
        "players",
        "participants",
        "ratings",
        "pairings",
        "bankruptcies",
        "wheel_spins",
        "double_spins",
        "tips",
        "bets",
        "prediction_positions",
        "prediction_trades",
        "tunnels",
        "dig_artifacts",
        "dig_achievements",
        "dig_actions",
        "mafia_games",
        "mafia_players",
        "mafia_actions",
        "recalibrations",
        "trivia_sessions",
        "disburse_vote_history",
        "protected_heroes",
    ]
    assert all(rows for rows in snapshot.values())
    assert all(row["guild_id"] == TEST_GUILD_ID for rows in snapshot.values() for row in rows)
    assert {row["discord_id"] for row in snapshot["players"]} == {
        ids["player1"],
        ids["player2"],
    }
    assert other["player1"] not in {
        row.get("discord_id") for rows in snapshot.values() for row in rows
    }
    assert [row["result"] for row in snapshot["wheel_spins"]] == [
        "earlier",
        "later",
    ]
    assert snapshot["participants"][0]["match_id"] == ids["match_id"]
    assert snapshot["prediction_positions"][0]["prediction_id"] == ids["prediction_id"]
    assert snapshot["mafia_players"][0]["game_id"] == ids["game_id"]

    assert "notes" not in snapshot["participants"][0]
    assert "enrichment_data" not in snapshot["participants"][0]
    assert "question" not in snapshot["prediction_positions"][0]
    assert "question" not in snapshot["prediction_trades"][0]
    assert "tunnel_name" not in snapshot["tunnels"][0]
    assert "miner_about" not in snapshot["tunnels"][0]
    assert "detail" not in snapshot["dig_actions"][0]
    assert "result" not in snapshot["mafia_actions"][0]


def test_load_snapshot_does_not_expose_active_mafia_roles(trivia_repo):
    with trivia_repo.connection() as conn:
        cursor = conn.execute(
            """
            INSERT INTO mafia_games (
                guild_id, game_date, phase, started_at, roster_size, status
            ) VALUES (?, 'active-game', 'NIGHT', 1, 4, 'ACTIVE')
            """,
            (TEST_GUILD_ID,),
        )
        game_id = int(cursor.lastrowid)
        conn.execute(
            """
            INSERT INTO mafia_players (guild_id, game_id, discord_id, role)
            VALUES (?, ?, 999, 'MAFIA')
            """,
            (TEST_GUILD_ID, game_id),
        )
        conn.execute(
            """
            INSERT INTO mafia_actions (
                guild_id, game_id, actor_id, target_id, action_type,
                phase, day_number, created_at, result
            ) VALUES (?, ?, 999, 1000, 'KILL', 'NIGHT', 1, 2, 'secret')
            """,
            (TEST_GUILD_ID, game_id),
        )

    snapshot = trivia_repo.load_snapshot(TEST_GUILD_ID)
    assert [row["game_id"] for row in snapshot["mafia_games"]] == [game_id]
    assert snapshot["mafia_players"] == []
    assert snapshot["mafia_actions"] == []
