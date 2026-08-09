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
        "**Match Players:**\n"
        f"<@1>: **+15** {JOPACOIN_EMOTE} (win +10, bet +5)\n"
        f"<@4>: **+5** {JOPACOIN_EMOTE} (excluded +5)\n"
        f"<@3>: **+2** {JOPACOIN_EMOTE} (play +5, bet −3)\n"
        f"<@2>: **+1** {JOPACOIN_EMOTE} (win +10, bet −10, streak +1)\n"
        "**Bettors:**\n"
        f"<@6>: **+9** {JOPACOIN_EMOTE} (bet +9)\n"
        f"<@5>: **−7** {JOPACOIN_EMOTE} (bet −7)"
    )
