"""
/trivia — Dota 2 trivia with streak mechanic and jopacoin rewards.
"""

from __future__ import annotations

import asyncio
import logging
import time
from dataclasses import dataclass, field

import discord
from discord import app_commands
from discord.ext import commands

from config import TRIVIA_ANSWER_TIMEOUT_SECONDS, TRIVIA_COOLDOWN_SECONDS, TRIVIA_REWARD_PER_QUESTION
from services.permissions import has_admin_permission
from services.trivia_questions import TriviaQuestion, generate_question, get_difficulty_tier
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.trivia")

DIFFICULTY_COLORS = {
    "easy": 0x43A047,    # Green
    "medium": 0xFFA000,  # Amber
    "hard": 0xE53935,    # Red
}

OPTION_LABELS = ["A", "B", "C", "D"]
OPTION_STYLES = [
    discord.ButtonStyle.primary,
    discord.ButtonStyle.primary,
    discord.ButtonStyle.primary,
    discord.ButtonStyle.primary,
]


@dataclass
class TriviaSession:
    user_id: int
    guild_id: int
    streak: int = 0
    total_jc: int = 0
    recent_categories: list[str] = field(default_factory=list)
    message: discord.Message | None = None       # current question message
    prev_message: discord.Message | None = None   # previous "correct" message (to delete)
    active: bool = True


def _question_embed(question: TriviaQuestion, question_num: int, streak: int, jc_earned: int) -> discord.Embed:
    """Build the embed for a trivia question."""
    tier = question.difficulty
    color = DIFFICULTY_COLORS.get(tier, 0x9E9E9E)
    embed = discord.Embed(
        title=f"Dota 2 Trivia — Question {question_num}",
        description=question.text,
        color=color,
    )
    # Add options as a field
    option_text = "\n".join(
        f"**{OPTION_LABELS[i]}.** {opt}" for i, opt in enumerate(question.options)
    )
    embed.add_field(name="Options", value=option_text, inline=False)
    if question.image_url:
        embed.set_thumbnail(url=question.image_url)
    embed.set_footer(text=f"Streak: {streak} | Difficulty: {tier.capitalize()} | JC earned: {jc_earned} | {TRIVIA_ANSWER_TIMEOUT_SECONDS}s to answer")
    return embed


def _correct_embed(question: TriviaQuestion, question_num: int, streak: int, jc_earned: int) -> discord.Embed:
    """Build the embed shown after a correct answer."""
    embed = discord.Embed(
        title=f"Question {question_num} — Correct! +{TRIVIA_REWARD_PER_QUESTION} {JOPACOIN_EMOTE}",
        description=f"**{OPTION_LABELS[question.correct_index]}.** {question.options[question.correct_index]}",
        color=0x43A047,
    )
    embed.set_footer(text=f"Streak: {streak} | JC earned: {jc_earned}")
    return embed


def _game_over_embed(question: TriviaQuestion | None, question_num: int, streak: int, jc_earned: int, timed_out: bool) -> discord.Embed:
    """Build the final game-over embed."""
    if timed_out:
        title = f"Question {question_num} — Time's up!"
    else:
        title = f"Question {question_num} — Wrong!"

    desc_parts = []
    if question:
        correct = question.options[question.correct_index]
        desc_parts.append(f"The correct answer was: **{OPTION_LABELS[question.correct_index]}.** {correct}")
        if question.explanation:
            desc_parts.append(f"\n*{question.explanation}*")

    embed = discord.Embed(
        title=title,
        description="\n".join(desc_parts) if desc_parts else None,
        color=0xE53935,
    )

    # Streak compliments
    summary = f"Final streak: **{streak}**\nTotal earned: **{jc_earned}** {JOPACOIN_EMOTE}"
    if streak >= 10:
        summary += "\nDota encyclopedia! Incredible run!"
    elif streak >= 5:
        summary += "\nImpressive knowledge!"

    embed.add_field(name="Game Over", value=summary, inline=False)
    return embed


class TriviaView(discord.ui.View):
    """View with A/B/C/D answer buttons for a trivia question."""

    def __init__(
        self,
        session: TriviaSession,
        question: TriviaQuestion,
        question_num: int,
        cog: TriviaCog,
    ):
        super().__init__(timeout=TRIVIA_ANSWER_TIMEOUT_SECONDS)
        self.session = session
        self.question = question
        self.question_num = question_num
        self.cog = cog
        self.answered = False

        for i, label in enumerate(OPTION_LABELS):
            if i < len(question.options):
                btn = discord.ui.Button(
                    label=f"{label}. {question.options[i][:72]}",
                    style=OPTION_STYLES[i],
                    custom_id=f"trivia_{session.user_id}_{question_num}_{i}",
                    row=i // 2,
                )
                btn.callback = self._make_callback(i)
                self.add_item(btn)

    def _make_callback(self, choice_index: int):
        async def callback(interaction: discord.Interaction):
            await self._handle_answer(interaction, choice_index)
        return callback

    async def _handle_answer(self, interaction: discord.Interaction, choice_index: int):
        if interaction.user.id != self.session.user_id:
            await interaction.response.send_message("This isn't your trivia session!", ephemeral=True)
            return
        if self.answered:
            return
        self.answered = True
        self.stop()

        is_correct = choice_index == self.question.correct_index

        if is_correct:
            self.session.streak += 1
            self.session.total_jc += TRIVIA_REWARD_PER_QUESTION
            self.session.recent_categories.append(self.question.category)

            # Award jopacoin
            try:
                player_service = self.cog.bot.player_service
                await asyncio.to_thread(
                    player_service.adjust_balance,
                    self.session.user_id,
                    self.session.guild_id,
                    TRIVIA_REWARD_PER_QUESTION,
                )
            except Exception:
                logger.exception("Failed to award trivia JC")

            # Delete the previous "correct" message if it exists (keep only last 2)
            if self.session.prev_message:
                try:
                    await self.session.prev_message.delete()
                except (discord.NotFound, discord.HTTPException):
                    pass

            # Edit current message to show it was answered correctly
            correct_embed = _correct_embed(
                self.question, self.question_num, self.session.streak, self.session.total_jc
            )
            try:
                await interaction.response.edit_message(embed=correct_embed, view=None)
            except discord.NotFound:
                pass

            # The current message (now showing "Correct!") becomes prev_message
            self.session.prev_message = self.session.message

            # Generate and send next question
            next_q = await asyncio.to_thread(
                generate_question,
                self.session.streak,
                self.session.recent_categories,
            )
            if next_q is None:
                # Ran out of questions somehow
                over_embed = _game_over_embed(None, self.question_num, self.session.streak, self.session.total_jc, False)
                over_embed.title = "Trivia — No more questions!"
                try:
                    await interaction.followup.send(embed=over_embed)
                except discord.HTTPException:
                    pass
                self.cog._end_session(self.session)
                return

            next_num = self.question_num + 1
            next_view = TriviaView(self.session, next_q, next_num, self.cog)
            next_embed = _question_embed(next_q, next_num, self.session.streak, self.session.total_jc)
            try:
                msg = await interaction.followup.send(embed=next_embed, view=next_view)
                self.session.message = msg
            except discord.HTTPException:
                logger.exception("Failed to send next trivia question")
                self.cog._end_session(self.session)
        else:
            # Wrong answer — delete previous "Correct!" message, game over
            if self.session.prev_message:
                try:
                    await self.session.prev_message.delete()
                except (discord.NotFound, discord.HTTPException):
                    pass
            over_embed = _game_over_embed(
                self.question, self.question_num, self.session.streak, self.session.total_jc, False
            )
            try:
                await interaction.response.edit_message(embed=over_embed, view=None)
            except discord.NotFound:
                pass
            self.cog._end_session(self.session)

    async def on_timeout(self):
        """Handle timeout — end the session."""
        if self.answered:
            return
        self.answered = True

        # Delete previous "Correct!" message
        if self.session.prev_message:
            try:
                await self.session.prev_message.delete()
            except (discord.NotFound, discord.HTTPException):
                pass

        over_embed = _game_over_embed(
            self.question, self.question_num, self.session.streak, self.session.total_jc, True
        )
        if self.session.message:
            try:
                await self.session.message.edit(embed=over_embed, view=None)
            except discord.NotFound:
                pass
        self.cog._end_session(self.session)

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        return interaction.user.id == self.session.user_id


class TriviaCog(commands.Cog):
    """Dota 2 trivia with streak rewards."""

    def __init__(self, bot: commands.Bot):
        self.bot = bot
        # Active sessions: (user_id, guild_id) -> TriviaSession
        self._sessions: dict[tuple[int, int], TriviaSession] = {}

    def _end_session(self, session: TriviaSession) -> None:
        session.active = False
        self._sessions.pop((session.user_id, session.guild_id), None)

    @app_commands.command(name="trivia", description="Test your Dota 2 knowledge! Earn 1 JC per correct answer.")
    @app_commands.checks.cooldown(1, 5.0)  # Rate limit: 1 per 5 seconds
    async def trivia(self, interaction: discord.Interaction):
        guild_id = interaction.guild.id if interaction.guild else 0
        user_id = interaction.user.id
        key = (user_id, guild_id)

        # Check for active session
        if key in self._sessions and self._sessions[key].active:
            await interaction.response.send_message(
                "You already have an active trivia session! Finish it first.", ephemeral=True
            )
            return

        if not await safe_defer(interaction):
            return

        # Check registration
        player_service = self.bot.player_service
        player = await asyncio.to_thread(player_service.get_player, user_id, guild_id)
        if not player:
            await safe_followup(interaction, content="You must be registered to play trivia. Use `/register` first.", ephemeral=True)
            return

        # Atomic cooldown check (admins bypass)
        is_admin = has_admin_permission(interaction)
        now = int(time.time())
        if not is_admin:
            claimed = await asyncio.to_thread(
                player_service.try_claim_trivia_session, user_id, guild_id, now, TRIVIA_COOLDOWN_SECONDS
            )
            if not claimed:
                last = await asyncio.to_thread(player_service.get_last_trivia_session, user_id, guild_id)
                if last:
                    next_available = last + TRIVIA_COOLDOWN_SECONDS
                    remaining = next_available - now
                    hours = remaining // 3600
                    minutes = (remaining % 3600) // 60
                    await safe_followup(
                        interaction,
                        content=f"Trivia is on cooldown! Next session available in **{hours}h {minutes}m**.",
                        ephemeral=True,
                    )
                else:
                    await safe_followup(interaction, content="Trivia is on cooldown.", ephemeral=True)
                return

        # Create session
        session = TriviaSession(user_id=user_id, guild_id=guild_id)
        self._sessions[key] = session

        # Generate first question
        question = await asyncio.to_thread(generate_question, 0, [])
        if question is None:
            await safe_followup(interaction, content="Failed to generate a trivia question. Try again later.", ephemeral=True)
            self._end_session(session)
            return

        embed = _question_embed(question, 1, 0, 0)
        view = TriviaView(session, question, 1, self)
        msg = await safe_followup(interaction, embed=embed, view=view)
        session.message = msg

    @trivia.error
    async def trivia_error(self, interaction: discord.Interaction, error: app_commands.AppCommandError):
        if isinstance(error, app_commands.CommandOnCooldown):
            await interaction.response.send_message(
                f"Slow down! Try again in {error.retry_after:.0f}s.", ephemeral=True
            )
        else:
            logger.exception("Trivia command error: %s", error)
            try:
                if not interaction.response.is_done():
                    await interaction.response.send_message("An error occurred.", ephemeral=True)
                else:
                    await interaction.followup.send("An error occurred.", ephemeral=True)
            except discord.HTTPException:
                pass


async def setup(bot: commands.Bot):
    await bot.add_cog(TriviaCog(bot))
