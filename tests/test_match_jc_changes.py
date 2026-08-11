from commands.match import MatchCommands
from services.match import recording_mixin
from utils.formatting import JOPACOIN_EMOTE


def test_bet_jc_deltas_net_all_stakes_payouts_and_deductions_per_user():
    distributions = {
        "winners": [
            {"discord_id": 1, "effective_bet": 10, "payout": 20},
        ],
        "losers": [
            {"discord_id": 1, "effective_bet": 4},
            {"discord_id": 2, "effective_bet": 5},
            {"discord_id": 3, "effective_bet": 7, "refunded": True},
        ],
        "bankruptcy_penalties": {1: 2},
        "vanity_taxes": {1: 1},
        "blood_pact_skims": {1: 1},
    }

    assert recording_mixin._calculate_bet_jc_deltas(distributions) == {1: 2, 2: -5, 3: 0}


def test_jc_changes_format_groups_players_and_bettors_sorted_by_net_change():
    record_result = {
        "winning_player_ids": [1, 2],
        "losing_player_ids": [3],
        "excluded_player_ids": [4],
        "bet_distributions": {
            "winners": [
                {
                    "discord_id": 1,
                    "amount": 5,
                    "effective_bet": 5,
                    "team": "radiant",
                    "multiplier": 2.5,
                },
                {
                    "discord_id": 6,
                    "amount": 6,
                    "effective_bet": 6,
                    "team": "radiant",
                    "multiplier": 2.5,
                },
            ],
            "losers": [
                {"discord_id": 2, "amount": 5, "effective_bet": 10},
                {"discord_id": 3, "amount": 3, "effective_bet": 3},
                {"discord_id": 5, "amount": 7, "effective_bet": 7},
            ],
        },
        "jc_changes": {
            5: {"bet": -7},
            6: {"bet": 9},
            3: {"payout": 5, "bet": -3},
            1: {"payout": 10, "bet": 5, "bet_blood_pact_skim": 1},
            4: {"payout": 5},
            2: {"payout": 10, "bet": -10, "streak": 1},
        },
    }

    assert MatchCommands._format_jc_changes(None, record_result) == (
        "\n\n🪙 **JC Changes:**\n"
        "**Final odds:** 🟢 Radiant 2.50x\n"
        "**Match Players:**\n"
        f"<@1>: **+15** {JOPACOIN_EMOTE} (win +10, bet +5 · stake 5)\n"
        f"<@4>: **+5** {JOPACOIN_EMOTE} (excluded +5)\n"
        f"<@3>: **+2** {JOPACOIN_EMOTE} (play +5, bet −3 · stake 3)\n"
        f"<@2>: **+1** {JOPACOIN_EMOTE} (win +10, bet −10 · stake 10, streak +1)\n"
        "**Bettors:**\n"
        f"<@6>: **+9** {JOPACOIN_EMOTE} (bet +9 · stake 6)\n"
        f"<@5>: **−7** {JOPACOIN_EMOTE} (bet −7 · stake 7)"
    )


def test_jc_changes_aggregates_multiple_effective_stakes_per_bettor():
    record_result = {
        "winning_player_ids": [],
        "losing_player_ids": [],
        "excluded_player_ids": [],
        "bet_distributions": {
            "winners": [
                {
                    "discord_id": 7,
                    "amount": 5,
                    "effective_bet": 10,
                    "team": "dire",
                    "multiplier": 1.75,
                },
                {
                    "discord_id": 7,
                    "amount": 10,
                    "effective_bet": 30,
                    "team": "dire",
                    "multiplier": 1.75,
                },
            ],
            "losers": [],
        },
        "jc_changes": {7: {"bet": 30}},
    }

    assert MatchCommands._format_jc_changes(None, record_result) == (
        "\n\n🪙 **JC Changes:**\n"
        "**Final odds:** 🔴 Dire 1.75x\n"
        "**Bettors:**\n"
        f"<@7>: **+30** {JOPACOIN_EMOTE} (bet +30 · stake 40)"
    )
