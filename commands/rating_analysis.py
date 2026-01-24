"""
Rating analysis commands for comparing Glicko-2 and OpenSkill systems.
"""

import asyncio
import logging

import discord
from discord import app_commands
from discord.ext import commands

from services.permissions import has_admin_permission
from services.rating_comparison_service import RatingComparisonService
from utils.drawing import (
    draw_calibration_curve,
    draw_prediction_over_time,
    draw_rating_comparison_chart,
)
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.rating_analysis")


class RatingAnalysisCommands(commands.Cog):
    """Admin commands for rating system analysis and comparison."""

    def __init__(
        self,
        bot: commands.Bot,
        match_service,
        match_repo,
        player_repo,
    ):
        self.bot = bot
        self.match_service = match_service
        self.match_repo = match_repo
        self.player_repo = player_repo

    @app_commands.command(
        name="ratinganalysis",
        description="Analyze and compare rating systems (Admin only)",
    )
    @app_commands.describe(
        action="Action to perform",
        user="Optional: Analyze specific player's rating data",
    )
    @app_commands.choices(
        action=[
            app_commands.Choice(name="compare - Show Glicko vs OpenSkill comparison", value="compare"),
            app_commands.Choice(name="calibration - Show calibration curves", value="calibration"),
            app_commands.Choice(name="trend - Show accuracy over time", value="trend"),
            app_commands.Choice(name="backfill - Recalculate OpenSkill from history", value="backfill"),
            app_commands.Choice(name="player - Show player's OpenSkill rating details", value="player"),
        ]
    )
    async def ratinganalysis(
        self,
        interaction: discord.Interaction,
        action: str,
        user: discord.Member | None = None,
    ):
        """Analyze and compare Glicko-2 and OpenSkill rating systems."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "This command is admin-only.",
                ephemeral=True,
            )
            return

        if action == "backfill":
            await self._handle_backfill(interaction)
        elif action == "compare":
            await self._handle_compare(interaction)
        elif action == "calibration":
            await self._handle_calibration(interaction)
        elif action == "trend":
            await self._handle_trend(interaction)
        elif action == "player":
            await self._handle_player(interaction, user)
        else:
            await interaction.response.send_message(
                f"Unknown action: {action}",
                ephemeral=True,
            )

    async def _handle_backfill(self, interaction: discord.Interaction):
        """Handle the backfill action - recalculate OpenSkill from historical data."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        await safe_followup(
            interaction,
            content="Starting OpenSkill backfill... This may take a while.",
            ephemeral=True,
        )

        # Run in executor to avoid blocking
        loop = asyncio.get_event_loop()
        try:
            result = await loop.run_in_executor(
                None,
                lambda: self.match_service.backfill_openskill_ratings(reset_first=True),
            )
        except Exception as e:
            logger.error(f"Backfill error: {e}")
            await safe_followup(
                interaction,
                content=f"Backfill failed: {e}",
                ephemeral=True,
            )
            return

        # Build result message
        embed = discord.Embed(
            title="OpenSkill Backfill Complete",
            color=discord.Color.green(),
        )
        embed.add_field(
            name="Matches Processed",
            value=f"{result['matches_processed']}/{result.get('total_matches', '?')}",
            inline=True,
        )
        embed.add_field(
            name="Players Updated",
            value=str(result["players_updated"]),
            inline=True,
        )

        if result.get("errors"):
            error_text = "\n".join(result["errors"][:5])
            if len(result["errors"]) > 5:
                error_text += f"\n...and {len(result['errors']) - 5} more"
            embed.add_field(name="Errors", value=error_text, inline=False)

        await safe_followup(interaction, embed=embed, ephemeral=True)

    async def _handle_compare(self, interaction: discord.Interaction):
        """Handle the compare action - show comparison statistics and chart."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        comparison_service = RatingComparisonService(
            self.match_repo, self.player_repo, self.match_service
        )

        # Run in executor
        loop = asyncio.get_event_loop()
        try:
            comparison_data = await loop.run_in_executor(
                None, comparison_service.get_comparison_summary
            )
        except Exception as e:
            logger.error(f"Comparison error: {e}")
            await safe_followup(
                interaction,
                content=f"Comparison failed: {e}",
            )
            return

        if "error" in comparison_data:
            await safe_followup(
                interaction,
                content=f"Comparison unavailable: {comparison_data['error']}",
            )
            return

        # Build embed
        embed = discord.Embed(
            title="Rating System Comparison",
            description=f"Analysis of {comparison_data['matches_analyzed']} matches",
            color=discord.Color.blue(),
        )

        glicko = comparison_data["glicko"]
        openskill = comparison_data["openskill"]
        comp = comparison_data["comparison"]

        # Brier Score (lower is better)
        brier_winner = comp["brier_winner"]
        brier_emoji = "" if brier_winner == "Glicko-2" else ""
        embed.add_field(
            name="Brier Score (Lower = Better)",
            value=(
                f"Glicko-2: **{glicko['brier_score']:.4f}** {brier_emoji if brier_winner == 'Glicko-2' else ''}\n"
                f"OpenSkill: **{openskill['brier_score']:.4f}** {brier_emoji if brier_winner == 'OpenSkill' else ''}"
            ),
            inline=True,
        )

        # Accuracy (higher is better)
        acc_winner = comp["accuracy_winner"]
        embed.add_field(
            name="Prediction Accuracy",
            value=(
                f"Glicko-2: **{glicko['accuracy']:.1%}**{'' if acc_winner == 'Glicko-2' else ''}\n"
                f"OpenSkill: **{openskill['accuracy']:.1%}**{'' if acc_winner == 'OpenSkill' else ''}"
            ),
            inline=True,
        )

        # Log Loss (lower is better)
        log_winner = "Glicko-2" if glicko["log_loss"] < openskill["log_loss"] else "OpenSkill"
        embed.add_field(
            name="Log Loss (Lower = Better)",
            value=(
                f"Glicko-2: **{glicko['log_loss']:.4f}**{'' if log_winner == 'Glicko-2' else ''}\n"
                f"OpenSkill: **{openskill['log_loss']:.4f}**{'' if log_winner == 'OpenSkill' else ''}"
            ),
            inline=True,
        )

        # Summary
        summary_parts = []
        if brier_winner == acc_winner:
            summary_parts.append(f"**{brier_winner}** performs better overall")
        else:
            summary_parts.append("Mixed results - neither system clearly dominates")

        brier_diff_pct = abs(glicko["brier_score"] - openskill["brier_score"]) / max(glicko["brier_score"], 0.001) * 100
        if brier_diff_pct < 5:
            summary_parts.append("Difference is marginal (<5%)")
        elif brier_diff_pct < 15:
            summary_parts.append(f"Moderate difference ({brier_diff_pct:.1f}%)")
        else:
            summary_parts.append(f"Significant difference ({brier_diff_pct:.1f}%)")

        embed.add_field(
            name="Summary",
            value="\n".join(summary_parts),
            inline=False,
        )

        # Generate chart
        try:
            chart = await loop.run_in_executor(
                None, lambda: draw_rating_comparison_chart(comparison_data)
            )
            file = discord.File(chart, filename="comparison.png")
            embed.set_image(url="attachment://comparison.png")
            await safe_followup(interaction, embed=embed, file=file)
        except Exception as e:
            logger.error(f"Chart generation error: {e}")
            await safe_followup(interaction, embed=embed)

    async def _handle_calibration(self, interaction: discord.Interaction):
        """Handle the calibration action - show calibration curves."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        comparison_service = RatingComparisonService(
            self.match_repo, self.player_repo, self.match_service
        )

        loop = asyncio.get_event_loop()
        try:
            curve_data = await loop.run_in_executor(
                None, comparison_service.get_calibration_curve_data
            )
        except Exception as e:
            logger.error(f"Calibration curve error: {e}")
            await safe_followup(
                interaction,
                content=f"Failed to generate calibration data: {e}",
            )
            return

        if "error" in curve_data:
            await safe_followup(
                interaction,
                content=f"Calibration unavailable: {curve_data['error']}",
            )
            return

        embed = discord.Embed(
            title="Rating System Calibration",
            description=(
                "Calibration curves show how well predicted probabilities match actual outcomes.\n"
                "A perfectly calibrated system would have all points on the diagonal line."
            ),
            color=discord.Color.blue(),
        )

        # Generate calibration curve chart
        try:
            chart = await loop.run_in_executor(
                None,
                lambda: draw_calibration_curve(curve_data["glicko"], curve_data["openskill"]),
            )
            file = discord.File(chart, filename="calibration.png")
            embed.set_image(url="attachment://calibration.png")
            await safe_followup(interaction, embed=embed, file=file)
        except Exception as e:
            logger.error(f"Calibration chart error: {e}")
            await safe_followup(
                interaction,
                content=f"Failed to generate calibration chart: {e}",
            )

    async def _handle_trend(self, interaction: discord.Interaction):
        """Handle the trend action - show accuracy over time."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        comparison_service = RatingComparisonService(
            self.match_repo, self.player_repo, self.match_service
        )

        loop = asyncio.get_event_loop()
        try:
            comparison_data = await loop.run_in_executor(
                None, comparison_service.get_comparison_summary
            )
        except Exception as e:
            logger.error(f"Trend analysis error: {e}")
            await safe_followup(
                interaction,
                content=f"Failed to analyze trends: {e}",
            )
            return

        if "error" in comparison_data:
            await safe_followup(
                interaction,
                content=f"Trend analysis unavailable: {comparison_data['error']}",
            )
            return

        match_data = comparison_data.get("match_data", [])
        if len(match_data) < 20:
            await safe_followup(
                interaction,
                content="Need at least 20 matches for trend analysis.",
            )
            return

        embed = discord.Embed(
            title="Prediction Accuracy Over Time",
            description=(
                f"Rolling accuracy of predictions across {len(match_data)} matches.\n"
                "Higher values = better predictions."
            ),
            color=discord.Color.blue(),
        )

        # Generate trend chart
        try:
            chart = await loop.run_in_executor(
                None, lambda: draw_prediction_over_time(match_data, window=20)
            )
            file = discord.File(chart, filename="trend.png")
            embed.set_image(url="attachment://trend.png")
            await safe_followup(interaction, embed=embed, file=file)
        except Exception as e:
            logger.error(f"Trend chart error: {e}")
            await safe_followup(
                interaction,
                content=f"Failed to generate trend chart: {e}",
            )

    async def _handle_player(
        self, interaction: discord.Interaction, user: discord.Member | None
    ):
        """Handle the player action - show a player's OpenSkill rating details."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        # Default to the command invoker if no user specified
        target = user or interaction.user
        discord_id = target.id

        # Fetch player data
        player = self.player_repo.get_by_id(discord_id)
        if not player:
            await safe_followup(
                interaction,
                content=f"{target.display_name} is not registered.",
            )
            return

        # Get OpenSkill rating
        os_data = self.player_repo.get_openskill_rating(discord_id)

        embed = discord.Embed(
            title=f"OpenSkill Rating: {target.display_name}",
            color=discord.Color.blue(),
        )

        if os_data:
            mu, sigma = os_data
            # Calculate ordinal (conservative skill estimate)
            ordinal = mu - 3 * sigma
            # Normalize to Glicko-like scale: μ * 50 + 250 (so μ=25 → 1500)
            normalized_rating = mu * 50 + 250
            # Check calibration status (sigma <= 4.0 is calibrated)
            is_calibrated = sigma <= 4.0

            # Normalized rating for easy comparison
            embed.add_field(
                name="Normalized Rating",
                value=f"**{normalized_rating:.0f}**",
                inline=True,
            )

            # Add Glicko comparison if available
            if player.glicko_rating is not None:
                embed.add_field(
                    name="Glicko-2 Rating",
                    value=f"**{player.glicko_rating:.0f}** ± {player.glicko_rd:.0f}",
                    inline=True,
                )

            embed.add_field(
                name="Calibrated",
                value="✓ Yes" if is_calibrated else f"No (need σ ≤ 4.0)",
                inline=True,
            )

            # OpenSkill native values
            embed.add_field(
                name="Skill (μ)",
                value=f"**{mu:.2f}**",
                inline=True,
            )
            embed.add_field(
                name="Uncertainty (σ)",
                value=f"**{sigma:.3f}**",
                inline=True,
            )
            embed.add_field(
                name="Ordinal (μ-3σ)",
                value=f"**{ordinal:.2f}**",
                inline=True,
            )

            # Get recent rating history for this player
            history = self.match_repo.get_player_openskill_history(discord_id, limit=5)
            if history:
                history_lines = []
                for h in history:
                    mu_change = h["os_mu_after"] - h["os_mu_before"]
                    sigma_change = h["os_sigma_after"] - h["os_sigma_before"]
                    won = h.get("won", None)
                    result_emoji = "🏆" if won else "❌" if won is False else "•"
                    weight_str = f" (w={h['fantasy_weight']:.1f})" if h.get("fantasy_weight") else ""
                    history_lines.append(
                        f"{result_emoji} μ: {mu_change:+.2f}, σ: {sigma_change:+.3f}{weight_str}"
                    )
                embed.add_field(
                    name="Recent OpenSkill Changes",
                    value="\n".join(history_lines),
                    inline=False,
                )
        else:
            embed.description = (
                "No OpenSkill rating data yet.\n\n"
                "OpenSkill ratings are calculated from enriched matches with fantasy data. "
                "Use `/ratinganalysis backfill` to calculate historical ratings."
            )

        await safe_followup(interaction, embed=embed)


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    match_service = getattr(bot, "match_service", None)
    match_repo = getattr(bot, "match_repo", None)
    player_repo = getattr(bot, "player_repo", None)

    if not all([match_service, match_repo, player_repo]):
        logger.warning("rating_analysis cog: required services not available, skipping")
        return

    await bot.add_cog(
        RatingAnalysisCommands(
            bot,
            match_service,
            match_repo,
            player_repo,
        )
    )
