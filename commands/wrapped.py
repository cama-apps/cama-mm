"""
Cama Wrapped commands - Monthly summary feature.

Provides /wrapped command and scheduled task for auto-generation.
"""

import logging
from datetime import datetime, timedelta

import discord
from discord import app_commands
from discord.ext import commands, tasks

from config import WRAPPED_CHECK_INTERVAL_HOURS, WRAPPED_ENABLED
from services.permissions import has_admin_permission
from utils.hero_lookup import get_hero_name
from utils.interaction_safety import safe_defer, safe_followup
from utils.wrapped_drawing import (
    draw_awards_grid,
    draw_wrapped_personal,
    draw_wrapped_summary,
)

logger = logging.getLogger("cama_bot.commands.wrapped")


def _get_hero_names_dict() -> dict[int, str]:
    """Build a dict of hero_id -> hero_name for image generation."""
    hero_names = {}
    # Hero IDs generally range from 1 to ~140
    for hero_id in range(1, 150):
        name = get_hero_name(hero_id)
        if name and name != "Unknown":
            hero_names[hero_id] = name
    return hero_names


class WrappedCog(commands.Cog):
    """Cog for Cama Wrapped monthly summaries."""

    def __init__(self, bot: commands.Bot):
        self.bot = bot
        self._hero_names: dict[int, str] | None = None
        # Start the daily check task
        if WRAPPED_ENABLED:
            self.check_monthly_wrapped.start()

    def cog_unload(self):
        """Clean up when cog is unloaded."""
        self.check_monthly_wrapped.cancel()

    @property
    def hero_names(self) -> dict[int, str]:
        """Lazily load hero names."""
        if self._hero_names is None:
            self._hero_names = _get_hero_names_dict()
        return self._hero_names

    @property
    def wrapped_service(self):
        """Get wrapped service from bot."""
        return getattr(self.bot, "wrapped_service", None)

    @tasks.loop(hours=WRAPPED_CHECK_INTERVAL_HOURS)
    async def check_monthly_wrapped(self):
        """
        Periodic check - if new month started, pre-generate wrapped stats.

        Runs on startup and then every WRAPPED_CHECK_INTERVAL_HOURS.
        Only generates on days 1-3 of a new month.
        """
        if not WRAPPED_ENABLED or not self.wrapped_service:
            return

        now = datetime.utcnow()

        # Only check first 3 days of month
        if now.day > 3:
            return

        # Calculate previous month
        first_of_month = now.replace(day=1)
        prev_month = first_of_month - timedelta(days=1)
        year_month = prev_month.strftime("%Y-%m")

        logger.info(f"Checking wrapped generation for {year_month}")

        for guild in self.bot.guilds:
            try:
                # Check if we can generate wrapped (validates cooldown and month completion)
                can_gen, reason = self.wrapped_service.can_generate_wrapped(
                    guild.id, year_month
                )
                if not can_gen:
                    logger.info(
                        f"Skipping wrapped for {guild.name} - {year_month}: {reason}"
                    )
                    continue

                # Pre-generate and cache stats
                wrapped = self.wrapped_service.get_server_wrapped(
                    guild.id, year_month, force_regenerate=False
                )
                if wrapped:
                    logger.info(
                        f"Auto-generated wrapped for {guild.name} - {year_month}: "
                        f"{wrapped.total_matches} matches, {wrapped.unique_players} players"
                    )
                else:
                    logger.info(
                        f"No data for wrapped in {guild.name} - {year_month}"
                    )
            except Exception as e:
                logger.error(f"Error generating wrapped for {guild.name}: {e}")

    @check_monthly_wrapped.before_loop
    async def before_check(self):
        """Wait until bot is ready before starting task."""
        await self.bot.wait_until_ready()
        logger.info(
            f"Wrapped check running on startup, then every {WRAPPED_CHECK_INTERVAL_HOURS}h"
        )

    @app_commands.command(name="wrapped", description="View monthly Cama Wrapped summary")
    @app_commands.describe(
        month="Month to view (YYYY-MM format, default: previous month)",
        user="View another user's personal wrapped",
    )
    async def wrapped(
        self,
        interaction: discord.Interaction,
        month: str | None = None,
        user: discord.User | None = None,
    ):
        """View wrapped summary for a month."""
        if not self.wrapped_service:
            await interaction.response.send_message(
                "Wrapped feature is not available.", ephemeral=True
            )
            return

        if not await safe_defer(interaction):
            return

        # Default to previous month
        if month is None:
            now = datetime.utcnow()
            first_of_month = now.replace(day=1)
            prev_month = first_of_month - timedelta(days=1)
            month = prev_month.strftime("%Y-%m")
        else:
            # Validate month format
            try:
                datetime.strptime(month, "%Y-%m")
            except ValueError:
                await safe_followup(
                    interaction,
                    content="Invalid month format. Use YYYY-MM (e.g., 2026-01).",
                    ephemeral=True,
                )
                return

        guild_id = interaction.guild.id if interaction.guild else None
        target_user = user or interaction.user

        # Generate or fetch wrapped
        try:
            server_wrapped = self.wrapped_service.get_server_wrapped(guild_id, month)

            if not server_wrapped:
                await safe_followup(
                    interaction,
                    content=f"No match data found for {month}.",
                    ephemeral=True,
                )
                return

            # Generate images
            files = []

            # Main summary image
            summary_buffer = draw_wrapped_summary(server_wrapped, self.hero_names)
            files.append(discord.File(summary_buffer, filename="wrapped_summary.png"))

            # Awards grid (top 6 awards)
            if server_wrapped.awards:
                awards_buffer = draw_awards_grid(server_wrapped.awards[:6])
                files.append(discord.File(awards_buffer, filename="wrapped_awards.png"))

            # Personal wrapped if viewing self or specific user
            player_wrapped = self.wrapped_service.get_player_wrapped(
                target_user.id, month, guild_id
            )
            if player_wrapped:
                personal_buffer = draw_wrapped_personal(player_wrapped, self.hero_names)
                files.append(
                    discord.File(personal_buffer, filename="wrapped_personal.png")
                )

            # Build embed
            embed = discord.Embed(
                title=f"Cama Wrapped - {server_wrapped.month_name}",
                color=discord.Color.gold(),
            )

            if player_wrapped:
                embed.description = (
                    f"**{target_user.display_name}'s Month:**\n"
                    f"{player_wrapped.games_played} games | "
                    f"{player_wrapped.wins}W-{player_wrapped.losses}L | "
                    f"{player_wrapped.win_rate*100:.0f}% WR"
                )

            embed.set_image(url="attachment://wrapped_summary.png")

            # Add award highlights to embed
            if server_wrapped.awards:
                award_text = []
                for award in server_wrapped.awards[:5]:
                    award_text.append(
                        f"{award.emoji} **{award.title}**: <@{award.discord_id}>"
                    )
                embed.add_field(
                    name="Top Awards",
                    value="\n".join(award_text),
                    inline=False,
                )

            await safe_followup(interaction, embed=embed, files=files)

        except Exception as e:
            logger.error(f"Error generating wrapped: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content=f"Error generating wrapped: {str(e)}",
                ephemeral=True,
            )

    @app_commands.command(
        name="testwrapped", description="Force regenerate wrapped (admin only)"
    )
    @app_commands.describe(
        month="Month to regenerate (YYYY-MM format)",
        force="Bypass already-generated check",
    )
    async def test_wrapped(
        self,
        interaction: discord.Interaction,
        month: str | None = None,
        force: bool = True,
    ):
        """Admin command to force regenerate wrapped."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "This command requires admin permissions.", ephemeral=True
            )
            return

        if not self.wrapped_service:
            await interaction.response.send_message(
                "Wrapped feature is not available.", ephemeral=True
            )
            return

        if not await safe_defer(interaction):
            return

        # Default to previous month
        if month is None:
            now = datetime.utcnow()
            first_of_month = now.replace(day=1)
            prev_month = first_of_month - timedelta(days=1)
            month = prev_month.strftime("%Y-%m")
        else:
            try:
                datetime.strptime(month, "%Y-%m")
            except ValueError:
                await safe_followup(
                    interaction,
                    content="Invalid month format. Use YYYY-MM (e.g., 2026-01).",
                    ephemeral=True,
                )
                return

        guild_id = interaction.guild.id if interaction.guild else None

        try:
            # Force regenerate
            wrapped = self.wrapped_service.get_server_wrapped(
                guild_id, month, force_regenerate=force
            )

            if not wrapped:
                await safe_followup(
                    interaction,
                    content=f"No match data found for {month}.",
                    ephemeral=True,
                )
                return

            # Generate summary image
            summary_buffer = draw_wrapped_summary(wrapped, self.hero_names)
            file = discord.File(summary_buffer, filename="wrapped_summary.png")

            embed = discord.Embed(
                title=f"Test Wrapped - {wrapped.month_name}",
                description=(
                    f"**Stats:**\n"
                    f"- {wrapped.total_matches} matches\n"
                    f"- {wrapped.total_wagered:,} JC wagered\n"
                    f"- {wrapped.unique_players} unique players\n"
                    f"- {wrapped.unique_heroes} unique heroes\n"
                    f"- {len(wrapped.awards)} awards generated"
                ),
                color=discord.Color.gold(),
            )
            embed.set_image(url="attachment://wrapped_summary.png")

            await safe_followup(interaction, embed=embed, file=file)

        except Exception as e:
            logger.error(f"Error in testwrapped: {e}", exc_info=True)
            await safe_followup(
                interaction, content=f"Error: {str(e)}", ephemeral=True
            )

    @app_commands.command(
        name="wrappedawards", description="View all awards from a wrapped"
    )
    @app_commands.describe(month="Month to view (YYYY-MM format, default: previous month)")
    async def wrapped_awards(
        self,
        interaction: discord.Interaction,
        month: str | None = None,
    ):
        """View detailed awards list from wrapped."""
        if not self.wrapped_service:
            await interaction.response.send_message(
                "Wrapped feature is not available.", ephemeral=True
            )
            return

        if not await safe_defer(interaction):
            return

        # Default to previous month
        if month is None:
            now = datetime.utcnow()
            first_of_month = now.replace(day=1)
            prev_month = first_of_month - timedelta(days=1)
            month = prev_month.strftime("%Y-%m")

        guild_id = interaction.guild.id if interaction.guild else None

        try:
            wrapped = self.wrapped_service.get_server_wrapped(guild_id, month)

            if not wrapped:
                await safe_followup(
                    interaction,
                    content=f"No wrapped data found for {month}.",
                    ephemeral=True,
                )
                return

            if not wrapped.awards:
                await safe_followup(
                    interaction,
                    content=f"No awards in wrapped for {month}.",
                    ephemeral=True,
                )
                return

            # Group awards by category
            by_category: dict[str, list] = {}
            for award in wrapped.awards:
                cat = award.category
                if cat not in by_category:
                    by_category[cat] = []
                by_category[cat].append(award)

            # Build embed with paginated awards
            embed = discord.Embed(
                title=f"Wrapped Awards - {wrapped.month_name}",
                color=discord.Color.gold(),
            )

            category_order = ["performance", "rating", "economy", "hero", "fun"]
            category_names = {
                "performance": "Performance Awards",
                "rating": "Rating Awards",
                "economy": "Economy Awards",
                "hero": "Hero Awards",
                "fun": "XD Awards",
            }

            for cat in category_order:
                if cat in by_category:
                    awards_text = []
                    for a in by_category[cat][:5]:  # Limit per category
                        awards_text.append(
                            f"{a.emoji} **{a.title}**: <@{a.discord_id}>\n"
                            f"  {a.stat_name}: {a.stat_value}"
                        )
                    embed.add_field(
                        name=category_names.get(cat, cat.title()),
                        value="\n".join(awards_text) or "None",
                        inline=False,
                    )

            embed.set_footer(text=f"Total: {len(wrapped.awards)} awards")

            await safe_followup(interaction, embed=embed)

        except Exception as e:
            logger.error(f"Error in wrappedawards: {e}", exc_info=True)
            await safe_followup(
                interaction, content=f"Error: {str(e)}", ephemeral=True
            )


async def setup(bot: commands.Bot):
    """Set up the Wrapped cog."""
    await bot.add_cog(WrappedCog(bot))
