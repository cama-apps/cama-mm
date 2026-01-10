"""
AI-powered natural language query command.

Allows users to ask questions about league data in natural language,
which are translated to SQL and executed safely.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import RateLimiter

if TYPE_CHECKING:
    from services.sql_query_service import SQLQueryService

logger = logging.getLogger("cama_bot.commands.ask")

# Rate limiter for AI queries (separate from global to control AI costs)
AI_RATE_LIMITER = RateLimiter()


class AskCommands(commands.Cog):
    """Commands for AI-powered natural language queries."""

    def __init__(
        self,
        bot: commands.Bot,
        sql_query_service: SQLQueryService,
    ):
        self.bot = bot
        self.sql_query_service = sql_query_service

    @app_commands.command(
        name="ask",
        description="Ask a question about league data in natural language",
    )
    @app_commands.describe(
        question="Your question (e.g., 'who has the highest win rate?', 'how many matches have been played?')"
    )
    async def ask(
        self,
        interaction: discord.Interaction,
        question: str,
    ):
        """
        Ask a natural language question about league data.

        The question is translated to SQL and executed safely against the database.

        Examples:
        - "Who has the highest win rate?"
        - "How many matches have been played?"
        - "What is the average rating?"
        - "Who plays pos 1 the most?"
        - "What's the best team duo?"
        """
        guild_id = interaction.guild_id

        # Rate limit check
        rl_result = AI_RATE_LIMITER.check(
            scope="ai_query",
            guild_id=guild_id or 0,
            user_id=interaction.user.id,
            limit=10,  # 10 requests
            per_seconds=60,  # per minute
        )
        if not rl_result.allowed:
            await interaction.response.send_message(
                f"Rate limited. Try again in {rl_result.retry_after_seconds:.0f} seconds.",
                ephemeral=True,
            )
            return

        # Defer since AI calls can take a while
        if not await safe_defer(interaction, ephemeral=False):
            return

        # Validate question length
        if len(question) < 5:
            await safe_followup(
                interaction,
                content="Please provide a more detailed question.",
                ephemeral=True,
            )
            return

        if len(question) > 500:
            await safe_followup(
                interaction,
                content="Question is too long. Please keep it under 500 characters.",
                ephemeral=True,
            )
            return

        # Execute query
        logger.info(f"User {interaction.user.id} asked: {question[:100]}")
        result = await self.sql_query_service.query(
            guild_id, question, asker_discord_id=interaction.user.id
        )

        # Build response embed
        embed = discord.Embed(
            title="AI Query Result",
            color=discord.Color.blue() if result.success else discord.Color.red(),
        )

        embed.add_field(
            name="Question",
            value=question[:256],
            inline=False,
        )

        if result.success:
            # Format results in a cleaner way
            formatted = result.format_for_discord(max_length=1024)
            embed.add_field(
                name="Answer",
                value=formatted,
                inline=False,
            )
            # Don't show SQL - it's internal implementation detail
        else:
            # Log the actual error internally
            logger.warning(f"Ask query failed for '{question}': {result.error}")
            embed.add_field(
                name="",
                value="I couldn't answer that. Try asking about player stats, matches, or the leaderboard.",
                inline=False,
            )

        await safe_followup(interaction, embed=embed)


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    sql_query_service = getattr(bot, "sql_query_service", None)

    if sql_query_service is None:
        logger.warning(
            "ask cog: sql_query_service not available (CEREBRAS_API_KEY not set?), skipping"
        )
        return

    await bot.add_cog(AskCommands(bot, sql_query_service))
    logger.info("AskCommands cog loaded")
