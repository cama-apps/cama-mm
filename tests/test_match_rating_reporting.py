"""Rating-system parity checks for match team-line reporting."""

from types import SimpleNamespace
from unittest.mock import MagicMock

from commands.match import MatchCommands
from domain.models.player import Player
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem


def test_openskill_balanced_team_line_uses_openskill_display_and_certainty():
    os_system = CamaOpenSkillSystem()
    match_service = SimpleNamespace(
        rating_system=CamaRatingSystem(),
        openskill_system=os_system,
    )
    cog = MatchCommands(MagicMock(), MagicMock(), match_service, MagicMock())
    player = Player(
        name="Player",
        preferred_roles=["1"],
        glicko_rating=1777.0,
        os_mu=45.0,
        os_sigma=4.0,
    )

    lines = cog._format_team_lines(
        SimpleNamespace(players=[player]),
        ["1"],
        [123],
        [player],
        guild=None,
        balancing_system="openskill",
    )

    expected_display = os_system.mu_to_display(player.os_mu)
    expected_certainty = os_system.get_certainty_percentage(player.os_sigma)
    assert f"[{expected_display} | {expected_certainty:.0f}%]" in lines[0]
    assert "1777" not in lines[0]


def test_glicko_balanced_team_line_keeps_glicko_display():
    match_service = SimpleNamespace(
        rating_system=CamaRatingSystem(),
        openskill_system=CamaOpenSkillSystem(),
    )
    cog = MatchCommands(MagicMock(), MagicMock(), match_service, MagicMock())
    player = Player(
        name="Player",
        preferred_roles=["1"],
        glicko_rating=1777.0,
        os_mu=45.0,
        os_sigma=4.0,
    )

    lines = cog._format_team_lines(
        SimpleNamespace(players=[player]),
        ["1"],
        [123],
        [player],
        guild=None,
        balancing_system="glicko",
    )

    assert "[1777]" in lines[0]


def test_notable_winner_compares_glicko_and_openskill_display_gains():
    match_service = SimpleNamespace(
        rating_system=CamaRatingSystem(),
        openskill_system=CamaOpenSkillSystem(),
        get_rating_history_for_match=MagicMock(
            return_value=[
                {
                    "discord_id": 1,
                    "rating_before": 1500.0,
                    "rating": 1520.0,
                    "os_mu_before": 40.0,
                    "os_mu_after": 40.2,
                    "expected_team_win_prob": 0.55,
                },
                {
                    "discord_id": 2,
                    "rating_before": 1500.0,
                    "rating": 1505.0,
                    "os_mu_before": 40.0,
                    "os_mu_after": 41.0,
                    "expected_team_win_prob": 0.55,
                },
            ]
        ),
    )
    cog = MatchCommands(MagicMock(), MagicMock(), match_service, MagicMock())

    winner_id, details = cog._get_notable_winner(42, [1, 2])

    assert winner_id == 2
    assert details["rating_change"] == 50
    assert details["rating_change_system"] == "openskill"
    assert details["glicko_rating_change"] == 5.0
    assert details["openskill_rating_change"] == 50
