"""Daily trivia generated from public, guild-scoped player statistics."""

from __future__ import annotations

import asyncio
import logging
import time
from dataclasses import dataclass

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_gamba_channel, require_guild
from config import (
    PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS,
    PLAYER_TRIVIA_COOLDOWN_SECONDS,
    PLAYER_TRIVIA_INCLUDE_SPICY,
    PLAYER_TRIVIA_QUESTION_COUNT,
    PLAYER_TRIVIA_RECENT_DAYS,
    PLAYER_TRIVIA_REWARD_PER_CORRECT,
)
from services.permissions import has_admin_permission
from services.player_trivia_service import PlayerTriviaQuestion
from utils.economy_scaling import scale_minigame_jc_delta
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import friendly_error, safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.player_trivia")

OPTION_LABELS = ("A", "B", "C", "D")


def _apply_daily_reward_event(bot, guild_id: int | None, amount: int) -> int:
    """Apply the active daily event after central minigame scaling."""
    event_service = getattr(bot, "economy_event_service", None)
    if event_service is None or amount <= 0:
        return amount
    return event_service.adjust_reward(guild_id, amount)


@dataclass
class PlayerTriviaSession:
    """In-memory UI state for a persisted player-trivia session."""

    session_id: int
    user_id: int
    guild_id: int
    user: discord.User | discord.Member
    questions: list[PlayerTriviaQuestion]
    current_index: int = 0
    score: int = 0
    total_jc: int = 0
    message: discord.Message | None = None
    active: bool = True


def _author(embed: discord.Embed, user: discord.User | discord.Member) -> None:
    avatar = getattr(getattr(user, "display_avatar", None), "url", None)
    if avatar:
        embed.set_author(name=user.display_name, icon_url=avatar)
    else:
        embed.set_author(name=user.display_name)


def _question_embed(
    session: PlayerTriviaSession,
    *,
    previous_result: str | None = None,
) -> discord.Embed:
    question = session.questions[session.current_index]
    number = session.current_index + 1
    embed = discord.Embed(
        title=f"Player Trivia — Question {number}/{len(session.questions)}",
        description=question.text,
        color=0x5865F2,
    )
    embed.add_field(
        name="Choose one",
        value="\n".join(
            f"**{OPTION_LABELS[index]}.** {option}" for index, option in enumerate(question.options)
        ),
        inline=False,
    )
    if previous_result:
        embed.add_field(name="Previous question result", value=previous_result, inline=False)
    elif number == 1:
        embed.add_field(
            name="Your daily set",
            value=(
                "Each player gets an independently generated set; "
                "some questions may overlap."
            ),
            inline=False,
        )
    _author(embed, session.user)
    embed.set_footer(
        text=(
            f"Score: {session.score}/{number - 1} | JC earned: {session.total_jc} | "
            f"{PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS}s to answer"
        )
    )
    return embed


def _answer_result(
    question: PlayerTriviaQuestion,
    *,
    is_correct: bool,
    reward: int,
) -> str:
    correct = (
        f"**{OPTION_LABELS[question.correct_index]}. {question.options[question.correct_index]}**"
    )
    if is_correct:
        lead = f"✅ Correct! **+{reward}** {JOPACOIN_EMOTE} The answer was {correct}."
    else:
        lead = f"❌ Not this time. The answer was {correct}."
    if question.explanation:
        lead += f"\n{question.explanation}"
    return lead


def _summary_embed(
    session: PlayerTriviaSession,
    *,
    title: str = "Player Trivia — Complete!",
    last_result: str | None = None,
) -> discord.Embed:
    total = len(session.questions)
    embed = discord.Embed(title=title, color=0x43A047 if session.score else 0x607D8B)
    if last_result:
        embed.description = last_result
    embed.add_field(
        name="Daily result",
        value=(
            f"Score: **{session.score}/{total}**\n"
            f"Earned: **{session.total_jc}** {JOPACOIN_EMOTE}\n"
            "Come back in 24 hours for another stat set."
        ),
        inline=False,
    )
    _author(embed, session.user)
    return embed


class PlayerTriviaView(discord.ui.View):
    """Four answer buttons for one frozen player-trivia question."""

    def __init__(self, session: PlayerTriviaSession, cog: PlayerTriviaCog):
        super().__init__(timeout=PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS)
        self.session = session
        self.cog = cog
        self.question_index = session.current_index
        self.answered = False

        question = session.questions[self.question_index]
        for index, _option in enumerate(question.options):
            button = discord.ui.Button(
                label=OPTION_LABELS[index],
                style=discord.ButtonStyle.primary,
                custom_id=(f"player_trivia:{session.session_id}:{self.question_index + 1}:{index}"),
                row=index // 2,
            )
            button.callback = self._callback(index)
            self.add_item(button)

    def _callback(self, selected_index: int):
        async def callback(interaction: discord.Interaction) -> None:
            await self._handle_answer(interaction, selected_index)

        return callback

    async def _handle_answer(self, interaction: discord.Interaction, selected_index: int) -> None:
        if interaction.user.id != self.session.user_id:
            await interaction.response.send_message(
                "This isn't your player-trivia session!", ephemeral=True
            )
            return
        if self.answered or not self.session.active:
            if not interaction.response.is_done():
                await interaction.response.defer()
            return

        # This assignment occurs before the first await, so two near-simultaneous
        # button callbacks cannot both enter settlement in this process. The
        # repository independently enforces the same idempotency across workers.
        self.answered = True
        self.stop()

        question = self.session.questions[self.question_index]
        reward = max(0, scale_minigame_jc_delta(PLAYER_TRIVIA_REWARD_PER_CORRECT))
        reward = _apply_daily_reward_event(
            self.cog.bot,
            self.session.guild_id,
            reward,
        )
        try:
            result = await asyncio.to_thread(
                self.cog.bot.player_trivia_service.settle_answer,
                self.session.session_id,
                self.question_index + 1,
                selected_index,
                reward,
                int(time.time()),
            )
        except Exception:
            logger.exception(
                "Failed to settle player-trivia answer for session %s",
                self.session.session_id,
            )
            await self.cog._finish_session(self.session, "error")
            await self.cog._edit_component_message(
                interaction,
                embed=_summary_embed(
                    self.session,
                    title="Player Trivia — Interrupted",
                    last_result=friendly_error("save that answer"),
                ),
                view=None,
            )
            return

        if not result:
            await self.cog._finish_session(self.session, "error")
            await self.cog._edit_component_message(
                interaction,
                embed=_summary_embed(
                    self.session,
                    title="Player Trivia — Interrupted",
                    last_result=(
                        "That round was already settled or was no longer active. "
                        "No duplicate reward was issued."
                    ),
                ),
                view=None,
            )
            return

        is_correct = bool(result["is_correct"])
        awarded = int(result.get("reward", 0))
        self.session.score = int(result.get("score", self.session.score))
        self.session.total_jc = int(result.get("jc_earned", self.session.total_jc))
        answer_text = _answer_result(
            question,
            is_correct=is_correct,
            reward=awarded,
        )

        is_last = self.question_index + 1 >= len(self.session.questions)
        if is_last or result.get("completed"):
            await self.cog._finish_session(self.session, "completed", persist=False)
            await self.cog._edit_component_message(
                interaction,
                embed=_summary_embed(self.session, last_result=answer_text),
                view=None,
            )
            return

        self.session.current_index += 1
        next_view = PlayerTriviaView(self.session, self.cog)
        await self.cog._edit_component_message(
            interaction,
            embed=_question_embed(self.session, previous_result=answer_text),
            view=next_view,
        )

    async def on_timeout(self) -> None:
        if self.answered or not self.session.active:
            return
        self.answered = True
        question = self.session.questions[self.question_index]
        answer_text = _answer_result(question, is_correct=False, reward=0)
        await self.cog._finish_session(self.session, "timed_out")
        if self.session.message:
            try:
                await self.session.message.edit(
                    embed=_summary_embed(
                        self.session,
                        title="Player Trivia — Time's up!",
                        last_result=answer_text,
                    ),
                    view=None,
                )
            except (discord.NotFound, discord.HTTPException):
                logger.info(
                    "Player-trivia message disappeared during timeout (session %s)",
                    self.session.session_id,
                )


class PlayerTriviaCog(commands.Cog):
    """A once-per-day quiz about this guild's recorded player history."""

    def __init__(self, bot: commands.Bot):
        self.bot = bot
        self._sessions: dict[tuple[int, int], PlayerTriviaSession] = {}

    async def _edit_component_message(
        self,
        interaction: discord.Interaction,
        *,
        embed: discord.Embed,
        view: discord.ui.View | None,
    ) -> None:
        try:
            await interaction.response.edit_message(embed=embed, view=view)
        except discord.InteractionResponded:
            if interaction.message:
                await interaction.message.edit(embed=embed, view=view)
        except (discord.NotFound, discord.HTTPException):
            logger.info("Player-trivia component message could not be edited")

    async def _finish_session(
        self,
        session: PlayerTriviaSession,
        status: str,
        *,
        persist: bool = True,
    ) -> None:
        if not session.active:
            return
        session.active = False
        self._sessions.pop((session.user_id, session.guild_id), None)
        if persist:
            try:
                await asyncio.to_thread(
                    self.bot.player_trivia_service.finish_session,
                    session.session_id,
                    status,
                    int(time.time()),
                )
            except Exception:
                logger.exception("Failed to finish player-trivia session %s", session.session_id)

    @app_commands.command(
        name="playertrivia",
        description="Play today's trivia set about this server's player stats.",
    )
    @app_commands.checks.cooldown(1, 5.0)
    @require_guild
    async def player_trivia(self, interaction: discord.Interaction) -> None:
        if not await require_gamba_channel(interaction):
            return

        guild_id = interaction.guild.id
        user_id = interaction.user.id
        key = (user_id, guild_id)
        current = self._sessions.get(key)
        if current and current.active:
            await interaction.response.send_message(
                "You already have an active player-trivia session!", ephemeral=True
            )
            return

        player = await asyncio.to_thread(self.bot.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You must be registered to play. Use `/player register` first.",
                ephemeral=True,
            )
            return

        is_admin = has_admin_permission(interaction)
        now = int(time.time())
        last_started = await asyncio.to_thread(
            self.bot.player_trivia_service.get_last_session_started,
            user_id,
            guild_id,
        )
        if (
            not is_admin
            and last_started is not None
            and now - int(last_started) < PLAYER_TRIVIA_COOLDOWN_SECONDS
        ):
            await interaction.response.send_message(
                "Player trivia is on cooldown! Your next set unlocks "
                f"<t:{int(last_started) + PLAYER_TRIVIA_COOLDOWN_SECONDS}:R>.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, thinking=True):
            return

        members = getattr(interaction.guild, "members", None)
        member_ids = (
            {member.id for member in members if not getattr(member, "bot", False)}
            if members
            else None
        )
        try:
            questions = await asyncio.to_thread(
                self.bot.player_trivia_service.generate_questions,
                user_id,
                guild_id,
                member_ids,
                PLAYER_TRIVIA_QUESTION_COUNT,
                PLAYER_TRIVIA_INCLUDE_SPICY,
                PLAYER_TRIVIA_RECENT_DAYS,
            )
        except Exception:
            logger.exception("Failed to generate player trivia for guild %s", guild_id)
            await safe_followup(
                interaction,
                content=friendly_error("build today's player-trivia set"),
                ephemeral=True,
            )
            return

        valid_questions = all(
            len(question.options) == 4
            and len(set(question.options)) == 4
            and 0 <= question.correct_index < 4
            for question in questions
        )
        if len(questions) < PLAYER_TRIVIA_QUESTION_COUNT or not valid_questions:
            await safe_followup(
                interaction,
                content=(
                    "There isn't enough eligible server history to build today's "
                    f"{PLAYER_TRIVIA_QUESTION_COUNT}-question set yet. No cooldown was used."
                ),
                ephemeral=True,
            )
            return

        questions = list(questions[:PLAYER_TRIVIA_QUESTION_COUNT])
        try:
            session_id = await asyncio.to_thread(
                self.bot.player_trivia_service.try_start_session,
                user_id,
                guild_id,
                questions,
                now,
                PLAYER_TRIVIA_COOLDOWN_SECONDS,
                bypass=is_admin,
            )
        except Exception:
            logger.exception("Failed to persist player-trivia session")
            await safe_followup(
                interaction,
                content=friendly_error("start today's player-trivia set"),
                ephemeral=True,
            )
            return

        if session_id is None:
            last_started = await asyncio.to_thread(
                self.bot.player_trivia_service.get_last_session_started,
                user_id,
                guild_id,
            )
            unlock = (
                int(last_started) + PLAYER_TRIVIA_COOLDOWN_SECONDS
                if last_started is not None
                else now + PLAYER_TRIVIA_COOLDOWN_SECONDS
            )
            await safe_followup(
                interaction,
                content=(
                    "Another player-trivia run already claimed today's set. "
                    f"Your next set unlocks <t:{unlock}:R>."
                ),
                ephemeral=True,
            )
            return

        session = PlayerTriviaSession(
            session_id=int(session_id),
            user_id=user_id,
            guild_id=guild_id,
            user=interaction.user,
            questions=questions,
        )
        self._sessions[key] = session
        view = PlayerTriviaView(session, self)
        try:
            message = await safe_followup(
                interaction,
                embed=_question_embed(session),
                view=view,
                allowed_mentions=discord.AllowedMentions.none(),
            )
            if message is None:
                raise RuntimeError("Discord returned no first-question message")
        except Exception:
            logger.exception(
                "Could not display first player-trivia question for session %s",
                session.session_id,
            )
            session.active = False
            self._sessions.pop(key, None)
            await asyncio.to_thread(
                self.bot.player_trivia_service.cancel_session_if_unanswered,
                session.session_id,
            )
            return
        session.message = message

    @player_trivia.error
    async def player_trivia_error(
        self, interaction: discord.Interaction, error: app_commands.AppCommandError
    ) -> None:
        if isinstance(error, app_commands.CommandOnCooldown):
            await interaction.response.send_message(
                f"Slow down! Try again in {error.retry_after:.0f}s.", ephemeral=True
            )
            return
        logger.exception("Player-trivia command error: %s", error)
        try:
            if interaction.response.is_done():
                await interaction.followup.send(
                    friendly_error("open player trivia"), ephemeral=True
                )
            else:
                await interaction.response.send_message(
                    friendly_error("open player trivia"), ephemeral=True
                )
        except discord.HTTPException:
            pass


async def setup(bot: commands.Bot) -> None:
    await bot.add_cog(PlayerTriviaCog(bot))
