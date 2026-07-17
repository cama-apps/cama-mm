from __future__ import annotations

import random
from collections import Counter
from unittest.mock import Mock

import pytest

from services.player_trivia_service import PlayerTriviaQuestion, PlayerTriviaService


class StableRng:
    """Minimal deterministic RNG that preserves candidate-builder order."""

    @staticmethod
    def sample(population, count):
        return list(population)[:count]

    @staticmethod
    def shuffle(_values):
        return None


def _players(count: int = 6, **overrides) -> list[dict]:
    rows = []
    for player_id in range(1, count + 1):
        row = {
            "discord_id": player_id,
            "username": f"P{player_id:02d}",
            "wins": 0,
            "losses": 0,
            "balance": 0,
        }
        row.update(overrides)
        rows.append(row)
    return rows


def _generate(
    snapshot: dict[str, list[dict]],
    *,
    recent=(),
    rng=None,
    count: int = 50,
    include_spicy: bool = False,
    current_member_ids=None,
):
    repo = Mock()
    repo.load_snapshot.return_value = snapshot
    repo.get_recent_question_keys.return_value = list(recent)
    service = PlayerTriviaService(repo, rng=rng or StableRng())
    questions = service.generate_questions(
        user_id=99,
        guild_id=7,
        current_member_ids=current_member_ids,
        count=count,
        include_spicy=include_spicy,
    )
    return questions, repo


def _answer(question: PlayerTriviaQuestion) -> str:
    return question.options[question.correct_index]


def _by_key(questions) -> dict[str, PlayerTriviaQuestion]:
    return {question.key: question for question in questions}


def _rich_snapshot() -> dict[str, list[dict]]:
    players = _players(8)
    tips = []
    wheel_spins = []
    trivia_sessions = []
    tunnels = []
    bankruptcies = []
    sequence = 1

    def value_with_winner(player_id: int, winner: int, scale: int = 1) -> int:
        return (100 - ((player_id - winner) % 8)) * scale

    for player_id in range(1, 9):
        for index in range(player_id):
            tips.append(
                {
                    "id": sequence,
                    "sender_id": player_id,
                    "recipient_id": player_id % 8 + 1,
                    "amount": player_id * 3,
                }
            )
            wheel_spins.append(
                {
                    "spin_id": sequence,
                    "discord_id": player_id,
                    "spin_time": sequence,
                    "outcome_code": f"OUTCOME_{index % 4}",
                }
            )
            sequence += 1

        tunnels.append(
            {
                "discord_id": player_id,
                "total_digs": value_with_winner(player_id, 2, 10),
                "max_depth": value_with_winner(player_id, 3, 9),
                "total_jc_earned": value_with_winner(player_id, 4, 100),
                "prestige_level": value_with_winner(player_id, 5),
                "best_run_score": value_with_winner(player_id, 6, 20),
                "pickaxe_tier": player_id,
                "stat_strength": player_id,
                "stat_smarts": player_id + 1,
                "stat_stamina": player_id + 2,
            }
        )
        bankruptcies.append({"discord_id": player_id, "bankruptcy_count": 9 - player_id})
        trivia_count = 9 - ((player_id - 7) % 8)
        for index in range(trivia_count):
            trivia_sessions.append(
                {
                    "id": sequence,
                    "discord_id": player_id,
                    "streak": (20 if player_id == 6 else player_id) + index,
                    "jc_earned": player_id * 2,
                }
            )
            sequence += 1
    return {
        "players": players,
        "tips": tips,
        "wheel_spins": wheel_spins,
        "trivia_sessions": trivia_sessions,
        "tunnels": tunnels,
        "bankruptcies": bankruptcies,
    }


def test_question_record_and_persistence_delegates_use_immutable_snapshot_shape():
    question = PlayerTriviaQuestion(
        key="matches:most_wins",
        category="matches",
        text="Who?",
        options=("A", "B", "C", "D"),
        correct_index=2,
        explanation="C had 12 wins.",
        spicy=True,
    )
    assert question.to_record() == {
        "question_key": "matches:most_wins",
        "category": "matches",
        "question_text": "Who?",
        "options": ["A", "B", "C", "D"],
        "correct_index": 2,
        "explanation": "C had 12 wins.",
        "spicy": True,
    }
    with pytest.raises(ValueError):
        PlayerTriviaQuestion("k", "c", "t", ("A", "A", "B", "C"), 0, "e")

    repo = Mock()
    service = PlayerTriviaService(repo)
    service.try_start_session(1, 2, [question], 100, 300, bypass=True)
    repo.try_start_session.assert_called_once_with(
        1, 2, [question.to_record()], 100, 300, bypass=True
    )
    service.settle_answer(4, 2, 1, 25, 101)
    repo.settle_answer.assert_called_once_with(4, 2, 1, 25, 101)
    service.finish_session(4, "completed", 102)
    repo.finish_session.assert_called_once_with(4, "completed", 102)
    service.cancel_session_if_unanswered(4)
    repo.cancel_session_if_unanswered.assert_called_once_with(4)


def test_generation_is_balanced_four_option_recent_aware_and_deterministic():
    snapshot = _rich_snapshot()
    first, _ = _generate(snapshot, rng=random.Random(144), count=10)
    second, _ = _generate(snapshot, rng=random.Random(144), count=10)

    assert first == second
    assert len(first) >= 7
    assert all(len(question.options) == 4 for question in first)
    assert all(len({option.casefold() for option in question.options}) == 4 for question in first)
    assert all(not question.spicy for question in first)
    assert max(Counter(question.category for question in first).values()) <= 2

    hidden = first[0].key
    without_recent, repo = _generate(
        snapshot,
        recent=[{"question_key": hidden}],
        rng=random.Random(144),
        count=20,
    )
    assert hidden not in {question.key for question in without_recent}
    assert repo.get_recent_question_keys.call_args.args[:2] == (99, 7)


def test_current_member_filter_removes_departed_players_before_aggregation():
    tips = []
    for player_id in range(1, 9):
        tips.append(
            {
                "sender_id": player_id,
                "recipient_id": 1,
                "amount": player_id * 10,
            }
        )
    questions, _ = _generate(
        {"players": _players(8), "tips": tips},
        current_member_ids=range(1, 8),
    )
    question = _by_key(questions)["tips:most_jc_sent"]
    assert _answer(question) == "<@7>"
    rendered = " ".join(
        [
            *(item.text for item in questions),
            *(item.explanation for item in questions),
            *(option for item in questions for option in item.options),
        ]
    )
    assert "<@8>" not in rendered


def test_displayed_win_rate_ties_are_rejected():
    players = _players(4)
    records = [(91, 9), (909, 90), (80, 20), (70, 30)]
    for row, (wins, losses) in zip(players, records, strict=True):
        row["wins"] = wins
        row["losses"] = losses
    questions, _ = _generate({"players": players})
    assert "matches:highest_win_rate" not in _by_key(questions)


def test_glicko_openskill_display_and_rank_discrepancies_use_same_scale():
    players = _players(4)
    ratings = [
        (2500, 25),  # OpenSkill display 0; display gap 2500; OS rank #4.
        (2000, 55),  # OpenSkill display 1500; OS rank #1.
        (1500, 53),  # OpenSkill display 1400; OS rank #2.
        (1000, 43),  # OpenSkill display 900; OS rank #3.
    ]
    for row, (glicko, mu) in zip(players, ratings, strict=True):
        row.update(
            {
                "wins": 20,
                "losses": 10,
                "glicko_rating": glicko,
                "glicko_rd": 50,
                "os_mu": mu,
                "os_sigma": 2,
            }
        )

    snapshot = {"players": players}
    display_questions, _ = _generate(
        snapshot,
        recent=["ratings:glicko_leader", "ratings:openskill_leader"],
    )
    display = _by_key(display_questions)["ratings:largest_display_gap"]
    assert _answer(display) == "<@1>"
    assert "2500 points" in display.explanation

    rank_questions, _ = _generate(
        snapshot,
        recent=[
            "ratings:glicko_leader",
            "ratings:openskill_leader",
            "ratings:largest_display_gap",
            "ratings:smallest_display_gap",
        ],
    )
    rank = _by_key(rank_questions)["ratings:largest_rank_gap"]
    assert _answer(rank) == "<@1>"
    assert "3 places" in rank.explanation


def test_spicy_questions_are_opt_in_but_neutral_loss_history_is_not_spicy():
    players = _players(4)
    for row, balance, lowest_ever in zip(
        players,
        [100, 200, 300, -500],
        [-10, -20, -30, -900],
        strict=True,
    ):
        row["balance"] = balance
        row["lowest_balance_ever"] = lowest_ever
    normal, _ = _generate({"players": players})
    spicy, _ = _generate({"players": players}, include_spicy=True)
    deep_spicy, _ = _generate(
        {"players": players},
        include_spicy=True,
        recent=["economy:highest_balance", "economy:lowest_balance"],
    )
    assert "economy:lowest_balance" not in _by_key(normal)
    assert _by_key(spicy)["economy:lowest_balance"].spicy is True
    assert "economy:deepest_historical_debt" not in _by_key(normal)
    assert _answer(_by_key(deep_spicy)["economy:deepest_historical_debt"]) == "<@4>"

    participants = []
    hero_sets = {
        1: [1, 2, 3, 4, 5],
        2: [6, 7, 8, 9],
        3: [10, 11, 12],
        4: [13, 13, 14],
    }
    match_id = 1
    for player_id, heroes in hero_sets.items():
        for hero_id in heroes:
            participants.append(
                {
                    "discord_id": player_id,
                    "match_id": match_id,
                    "match_date": match_id,
                    "hero_id": hero_id,
                    "won": 0,
                }
            )
            match_id += 1
    hero_questions, _ = _generate({"players": _players(4), "participants": participants})
    lost = _by_key(hero_questions)["heroes:most_distinct_lost"]
    assert _answer(lost) == "<@1>"
    assert lost.spicy is False


def test_tip_amount_and_transaction_leaders_are_separate_statistics():
    tips = [{"sender_id": 1, "recipient_id": 6, "amount": 1000}]
    for player_id, count, amount in [(2, 5, 10), (3, 4, 5), (4, 3, 2), (5, 2, 1)]:
        tips.extend(
            {"sender_id": player_id, "recipient_id": 6, "amount": amount} for _ in range(count)
        )
    questions, _ = _generate({"players": _players(6), "tips": tips})
    keyed = _by_key(questions)
    assert _answer(keyed["tips:most_jc_sent"]) == "<@1>"
    assert _answer(keyed["tips:most_transactions_sent"]) == "<@2>"


def test_exact_wheel_outcome_supports_lightning_frequency_and_ignores_null_codes():
    rows = []
    spin_id = 1
    for player_id, count in [(1, 100), (2, 4), (3, 2), (4, 1)]:
        for _ in range(count):
            rows.append(
                {
                    "spin_id": spin_id,
                    "discord_id": player_id,
                    "spin_time": spin_id,
                    "outcome_code": "LIGHTNING",
                }
            )
            spin_id += 1
    for _ in range(200):
        rows.append(
            {
                "spin_id": spin_id,
                "discord_id": 6,
                "spin_time": spin_id,
                "outcome_code": None,
            }
        )
        spin_id += 1
    questions, _ = _generate(
        {"players": _players(6), "wheel_spins": rows},
        recent=[
            "wheel:most_spins",
            "wheel:most_common_outcome:1",
            "wheel:most_common_outcome:2",
        ],
    )
    keyed = _by_key(questions)
    leader = keyed["wheel:exact_outcome_leader:lightning"]
    count = keyed["wheel:exact_outcome_count:lightning:1"]
    assert _answer(leader) == "<@1>"
    assert _answer(count) == "100 times"
    assert "Lightning" in count.text
    assert all("none" not in question.key.casefold() for question in questions)


def test_finalized_disbursement_history_can_ask_about_burn():
    rows = []
    proposal_id = 1
    for _ in range(3):
        rows.append(
            {
                "proposal_id": proposal_id,
                "discord_id": 1,
                "vote_method": "burn",
                "proposal_outcome": "burn",
                "finalized_at": proposal_id,
            }
        )
        proposal_id += 1
    for player_id, method in [
        (2, "even"),
        (3, "proportional"),
        (4, "neediest"),
        (5, "stimulus"),
    ]:
        rows.append(
            {
                "proposal_id": proposal_id,
                "discord_id": player_id,
                "vote_method": method,
                "proposal_outcome": method,
                "finalized_at": proposal_id,
            }
        )
        proposal_id += 1
    questions, _ = _generate(
        {"players": _players(5), "disburse_vote_history": rows},
        recent=["disburse:most_finalized_ballots"],
    )
    question = _by_key(questions)["disburse:most_common_vote:1"]
    assert _answer(question) == "Burn"
    assert "finalized" in question.text.casefold()


def test_prediction_questions_include_market_descriptor_without_creator_metadata():
    positions = []
    pnl_signs = {
        1: [-1, 1, 1],
        2: [1, 1, 1],
        3: [-1, -1, 1],
        4: [-1, -1, -1],
    }
    for player_id, signs in pnl_signs.items():
        for offset, sign in enumerate(signs, 1):
            positions.append(
                {
                    "prediction_id": player_id * 10 + offset,
                    "discord_id": player_id,
                    "status": "resolved",
                    "outcome": "yes",
                    "yes_contracts": 1,
                    "yes_cost_basis_total": 0 if sign > 0 else 20,
                    "no_contracts": 0,
                    "no_cost_basis_total": 0,
                    "creator_id": 6,
                    "question": f"Will market {player_id * 10 + offset} resolve YES?",
                    "metadata": "SECRET META",
                }
            )
    questions, _ = _generate(
        {"players": _players(6), "prediction_positions": positions},
        recent=["predictions:most_wins", "predictions:best_total_pnl"],
    )
    loss = _by_key(questions)["predictions:loss_market:1:11"]
    assert _answer(loss) == "Market #11 — Will market 11 resolve YES?"
    assert "<@1>" in loss.text
    assert loss.spicy is False
    rendered = " ".join(
        [
            *(question.text for question in questions),
            *(question.explanation for question in questions),
            *(option for question in questions for option in question.options),
        ]
    ).casefold()
    assert "creator" not in rendered
    assert "secret meta" not in rendered


def test_mafia_questions_use_only_resolved_non_cancelled_games():
    games = [
        {
            "game_id": game_id,
            "phase": "RESOLVED",
            "status": "COMPLETED",
            "winner": "TOWN",
            "mvp_id": 1 if game_id == 1 else None,
        }
        for game_id in range(1, 6)
    ]
    players = []
    for game_id in range(1, 6):
        for player_id, games_played in [(1, 5), (2, 4), (3, 3), (4, 2)]:
            if game_id <= games_played:
                players.append(
                    {
                        "game_id": game_id,
                        "discord_id": player_id,
                        "role": "TOWNIE",
                    }
                )
    for game_id in range(100, 120):
        games.append(
            {
                "game_id": game_id,
                "phase": "NIGHT",
                "status": "ACTIVE",
                "winner": None,
            }
        )
        players.append({"game_id": game_id, "discord_id": 5, "role": "MAFIA"})
    games.append(
        {
            "game_id": 200,
            "phase": "RESOLVED",
            "status": "CANCELLED",
            "winner": "TOWN",
        }
    )
    players.append({"game_id": 200, "discord_id": 6, "role": "TOWNIE"})

    questions, _ = _generate(
        {
            "players": _players(6),
            "mafia_games": games,
            "mafia_players": players,
        }
    )
    question = _by_key(questions)["mafia:most_games"]
    assert _answer(question) == "<@1>"
    assert "resolved" in question.text.casefold()
