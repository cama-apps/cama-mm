"""
Tests for the /calibration command (public access).
"""

import ast
import inspect
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest


class TestCalibrationCommandAccess:
    """Tests verifying the /calibration command is publicly accessible."""

    def test_calibration_command_has_no_admin_check(self):
        """Verify the calibration command does not contain an admin permission check.

        The /calibration command was made public - this test ensures no admin check
        is present in the command handler by inspecting the source code.
        """
        info_py_path = Path(__file__).parent.parent / "commands" / "info.py"
        source = info_py_path.read_text(encoding="utf-8")

        # Parse the AST to find the calibration function
        tree = ast.parse(source)

        # Find the calibration method
        calibration_func = None
        for node in ast.walk(tree):
            if isinstance(node, ast.AsyncFunctionDef) and node.name == "calibration":
                calibration_func = node
                break

        assert calibration_func is not None, "Could not find calibration function"

        # Get the function source lines
        func_start = calibration_func.lineno
        func_end = calibration_func.end_lineno
        source_lines = source.splitlines()
        func_source = "\n".join(source_lines[func_start - 1 : func_end])

        # Verify no admin check exists in the function
        assert "has_admin_permission" not in func_source, (
            "calibration command should not have admin permission check - "
            "it was made public"
        )
        assert "admin-only" not in func_source.lower(), (
            "calibration command should not reference admin-only - "
            "it was made public"
        )

    def test_calibration_command_description_is_public(self):
        """Verify the command description does not mention admin-only."""
        info_py_path = Path(__file__).parent.parent / "commands" / "info.py"
        source = info_py_path.read_text(encoding="utf-8")

        # Find the @app_commands.command decorator for calibration
        # Look for: @app_commands.command(name="calibration", description="...")
        import re

        pattern = r'@app_commands\.command\(\s*name="calibration"\s*,\s*description="([^"]+)"'
        match = re.search(pattern, source)

        assert match is not None, "Could not find calibration command decorator"

        description = match.group(1)
        assert "admin" not in description.lower(), (
            f"calibration command description should not mention admin: {description}"
        )

    def test_calibration_command_docstring_is_public(self):
        """Verify the function docstring does not mention admin-only."""
        # Import the actual command to check its docstring
        from commands.info import InfoCommands

        # Get the calibration method
        calibration_method = getattr(InfoCommands, "calibration", None)
        assert calibration_method is not None, "calibration method not found"

        docstring = inspect.getdoc(calibration_method.callback)
        assert docstring is not None, "calibration method should have a docstring"
        assert "admin" not in docstring.lower(), (
            f"calibration docstring should not mention admin: {docstring}"
        )


class TestCalibrationStatsIntegration:
    """Integration tests for calibration stats computation."""

    def test_calibration_stats_with_empty_data(self):
        """Verify calibration stats can be computed with no players."""
        from utils.rating_insights import compute_calibration_stats

        stats = compute_calibration_stats(
            players=[],
            match_count=0,
            match_predictions=[],
            rating_history_entries=[],
        )

        # Should return valid structure even with empty data
        assert "rating_buckets" in stats
        assert "rd_tiers" in stats
        assert "prediction_quality" in stats
        assert "rating_movement" in stats
        assert stats["avg_rating"] is None
        assert stats["avg_rd"] is None

    def test_calibration_stats_with_single_player(self):
        """Verify calibration stats work with a single player."""
        from domain.models.player import Player
        from utils.rating_insights import compute_calibration_stats

        player = Player(
            name="TestPlayer",
            glicko_rating=1200.0,
            glicko_rd=100.0,
            glicko_volatility=0.06,
            wins=5,
            losses=3,
            initial_mmr=3500,
            discord_id=12345,
        )

        stats = compute_calibration_stats(
            players=[player],
            match_count=8,
            match_predictions=[],
            rating_history_entries=[],
        )

        # Should categorize single player correctly
        assert stats["rating_buckets"]["Divine"] == 1  # 1200 falls in Divine range
        assert stats["rd_tiers"]["Settling"] == 1  # RD 100 falls in Settling range
        assert stats["avg_rating"] == 1200.0
        assert stats["avg_rd"] == 100.0


class TestIndividualCalibrationProfile:
    """Tests for the Rating Profile field of /calibration <user>."""

    @pytest.mark.asyncio
    async def test_unrated_player_rating_profile_not_duplicated(self):
        """An unrated player (falsy volatility) must not see Rating/Tier twice.

        Regression: a malformed ternary bound if/else to only the Volatility
        literal, so a falsy glicko_volatility produced "Rating .. Tier .. Rating
        .. Tier" - the Rating/Tier lines rendered twice.
        """
        from commands.info import InfoCommands
        from domain.models.player import Player
        from rating_system import CamaRatingSystem

        # Player with no volatility -> falsy branch of the buggy ternary.
        player = Player(
            name="Unrated",
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=None,
            initial_mmr=3000,
            discord_id=777,
        )

        player_service = MagicMock()
        player_service.get_player.return_value = player
        player_service.get_all.return_value = [player]
        player_service.get_openskill_rating.return_value = None

        cog = InfoCommands(
            bot=MagicMock(),
            player_service=player_service,
            match_service=None,  # skips history-dependent branches
        )

        # The real safe_followup runs and awaits interaction.followup.send;
        # reading the embed off an AsyncMock avoids a monkeypatch that a
        # parallel cog-reload could defeat (test-isolation flake).
        interaction = MagicMock()
        interaction.guild = MagicMock()
        interaction.guild.id = 12345
        interaction.followup.send = AsyncMock()
        user = MagicMock()
        user.id = 777
        user.display_name = "Unrated"
        user.mention = "<@777>"

        await cog._show_individual_calibration(interaction, user, CamaRatingSystem())

        interaction.followup.send.assert_called_once()
        embed = interaction.followup.send.call_args.kwargs["embed"]
        assert embed is not None
        profile_field = next(
            (f for f in embed.fields if "Rating Profile" in f.name), None
        )
        assert profile_field is not None
        # The Rating and Tier lines must each appear exactly once.
        assert profile_field.value.count("**Rating:**") == 1
        assert profile_field.value.count("**Tier:**") == 1
        assert "**RD:** 350" in profile_field.value


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
