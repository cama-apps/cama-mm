from types import SimpleNamespace

from services.vanity_tax_service import VanityTaxService

GUILD_ID = 123


def _member(discord_id: int, nickname: str | None):
    return SimpleNamespace(id=discord_id, nick=nickname)


def test_refresh_taxes_only_members_without_server_nicknames():
    service = VanityTaxService()

    service.refresh_guild(
        GUILD_ID,
        [
            _member(1, None),
            _member(2, "Real Name"),
        ],
    )

    assert service.calculate_tax(1, GUILD_ID, 199) == 9  # 5% floored
    assert service.calculate_tax(2, GUILD_ID, 199) == 0


def test_member_updates_toggle_taxability_and_removal_fails_open():
    service = VanityTaxService()
    service.refresh_guild(GUILD_ID, [_member(1, None)])

    service.update_member(GUILD_ID, 1, "Real Name")
    assert service.calculate_tax(1, GUILD_ID, 500) == 0

    service.update_member(GUILD_ID, 1, None)
    assert service.calculate_tax(1, GUILD_ID, 500) == 25

    service.remove_member(GUILD_ID, 1)
    assert service.calculate_tax(1, GUILD_ID, 500) == 0


def test_tax_floors_five_percent_and_ignores_unknown_or_nonpositive_profit():
    service = VanityTaxService()
    service.refresh_guild(GUILD_ID, [_member(1, None)])

    # Floor keeps tiny profits (< 20 JC) untaxed; the 5% rate is what makes
    # the tax visible at this economy's payout sizes (1% floored to 0).
    assert service.calculate_tax(1, GUILD_ID, 19) == 0
    assert service.calculate_tax(1, GUILD_ID, 99) == 4
    assert service.calculate_tax(1, GUILD_ID, 100) == 5
    assert service.calculate_tax(1, GUILD_ID, 0) == 0
    assert service.calculate_tax(1, GUILD_ID, -100) == 0
    assert service.calculate_tax(999, GUILD_ID, 1_000) == 0
    assert service.calculate_tax(1, 999, 1_000) == 0


def test_settlement_announcements_show_vanity_tax():
    from commands.match import MatchCommands
    from commands.predictions import _build_resolution_announcement_chunks

    match_text = MatchCommands._format_bet_distribution(
        None,
        [{"discord_id": 1, "payout": 200, "amount": 100}],
        [],
        {},
        {1: 1},
    )
    assert "won 199" in match_text
    assert "−1" in match_text
    assert "vanity tax" in match_text

    prediction_text = "\n".join(
        _build_resolution_announcement_chunks(
            5,
            "yes",
            [
                {
                    "discord_id": 1,
                    "cost_basis": 100,
                    "yes_contracts": 2,
                    "no_contracts": 0,
                    "payout": 199,
                    "profit": 99,
                }
            ],
            0,
            1,
        )
    )
    assert "1 JC vanity tax" in prediction_text
