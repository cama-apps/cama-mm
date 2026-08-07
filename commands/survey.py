"""Admin-managed, one-off surveys delivered and answered through Discord DMs."""

from __future__ import annotations

import asyncio
import hashlib
import logging
import weakref
from datetime import UTC, datetime

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_guild
from domain.models.survey import (
    MAX_SURVEY_PROMPT_LENGTH,
    MAX_SURVEY_TITLE_LENGTH,
    MAX_TEXT_ANSWER_LENGTH,
    Survey,
    SurveyQuestion,
    SurveyQuestionType,
    SurveyResults,
    SurveySession,
    SurveyStatus,
    SurveyTargetType,
)
from services.permissions import has_admin_permission
from utils.embed_safety import truncate_field
from utils.formatting import escape_discord_text
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.survey")

_ALLOWED_MENTIONS = discord.AllowedMentions.none()
_DELIVERY_BATCH_SIZE = 25
_DELIVERY_CONCURRENCY = 5
_DELIVERY_STALE_SECONDS = 300
_DELIVERY_RECEIPT_GRACE_SECONDS = 30
_DELIVERY_RECEIPT_SCAN_LIMIT = 500
_RESULTS_TIMEOUT_SECONDS = 600
_TEXT_RESULTS_PER_PAGE = 4
_EMBED_CONTENT_BUDGET = 5_900
_PREVIEW_QUESTIONS_PER_PAGE = 5
_REVIEW_PROMPT_PREVIEW_LENGTH = 150
_REVIEW_ANSWER_PREVIEW_LENGTH = 350

QUESTION_TYPE_CHOICES = [
    app_commands.Choice(name="NPS (0–10)", value=SurveyQuestionType.NPS.value),
    app_commands.Choice(name="Rating (1–5)", value=SurveyQuestionType.RATING.value),
    app_commands.Choice(name="Free text", value=SurveyQuestionType.TEXT.value),
]
TARGET_TYPE_CHOICES = [
    app_commands.Choice(
        name="All registered players",
        value=SurveyTargetType.ALL_REGISTERED.value,
    ),
    app_commands.Choice(name="One member", value=SurveyTargetType.MEMBER.value),
    app_commands.Choice(name="A role", value=SurveyTargetType.ROLE.value),
]
STATUS_CHOICES = [
    app_commands.Choice(name="Draft", value=SurveyStatus.DRAFT.value),
    app_commands.Choice(name="Open", value=SurveyStatus.OPEN.value),
    app_commands.Choice(name="Closed", value=SurveyStatus.CLOSED.value),
]


def _answer_by_question(session: SurveySession) -> dict[int, object]:
    return {answer.question_id: answer for answer in session.answers}


def _question_number(session: SurveySession, question_id: int) -> int:
    return next(
        (
            index
            for index, question in enumerate(session.questions, start=1)
            if question.question_id == question_id
        ),
        1,
    )


def _question_embed(
    session: SurveySession,
    question: SurveyQuestion,
) -> discord.Embed:
    number = _question_number(session, question.question_id)
    kind = {
        SurveyQuestionType.NPS: "Choose a score from 0 to 10.",
        SurveyQuestionType.RATING: "Choose a rating from 1 to 5.",
        SurveyQuestionType.TEXT: "Write your response in the private text form.",
    }[question.question_type]
    embed = discord.Embed(
        title=f"Survey — {escape_discord_text(session.survey.title)}",
        description=escape_discord_text(question.prompt),
        color=0x5865F2,
    )
    embed.add_field(name="How to answer", value=kind, inline=False)
    if session.survey.target_type is SurveyTargetType.MEMBER:
        # A single-recipient survey cannot honor an anonymity promise: the
        # admin knows exactly whose answers the results contain.
        privacy = (
            "This survey was sent only to you, so your answers are shared "
            "with the survey admins and are not anonymous."
        )
    else:
        privacy = (
            "Admins receive anonymous, per-question results. "
            "Your answers are not shown with your Discord identity."
        )
    embed.add_field(name="Privacy", value=privacy, inline=False)
    requirement = "Required" if question.required else "Optional"
    edit_suffix = " • Editing from review" if session.recipient.in_review else ""
    embed.set_footer(
        text=f"Question {number}/{len(session.questions)} • {requirement}{edit_suffix}"
    )
    return embed


def _review_embed(session: SurveySession) -> discord.Embed:
    answers = _answer_by_question(session)
    embed = discord.Embed(
        title=f"Review — {escape_discord_text(session.survey.title)}",
        description=(
            "Review your answers, edit any question you want, then submit. "
            "Nothing appears in admin results until you submit."
        ),
        color=0x57F287,
    )
    for question in session.questions:
        answer = answers.get(question.question_id)
        if answer is None:
            rendered = "Not answered"
        elif getattr(answer, "skipped", False):
            rendered = "Skipped"
        elif getattr(answer, "text_value", None) is not None:
            rendered = escape_discord_text(str(answer.text_value))
        else:
            rendered = str(getattr(answer, "numeric_value", ""))
        embed.add_field(
            name=truncate_field(
                f"{question.position}. {escape_discord_text(question.prompt)}",
                _REVIEW_PROMPT_PREVIEW_LENGTH,
            ),
            value=truncate_field(rendered, _REVIEW_ANSWER_PREVIEW_LENGTH),
            inline=False,
        )
    embed.set_footer(text="Your response has not been submitted yet.")
    return embed


def _preview_pages(
    survey: Survey,
    questions: tuple[SurveyQuestion, ...] | list[SurveyQuestion],
) -> list[discord.Embed]:
    """Render a complete draft preview without exceeding Discord embed limits."""
    if not questions:
        embed = discord.Embed(
            title=f"#{survey.survey_id} — {escape_discord_text(survey.title)}",
            description=f"Status: **{survey.status.value.title()}**",
            color=0x5865F2,
        )
        embed.add_field(name="Questions", value="No questions yet.", inline=False)
        return [embed]

    pages: list[discord.Embed] = []
    embed: discord.Embed | None = None
    questions_on_page = 0
    for question in questions:
        requirement = "required" if question.required else "optional"
        field_name = (
            f"{question.position}. {question.question_type.value.upper()} "
            f"({requirement}) • ID {question.question_id}"
        )
        prompt_chunks = _escaped_field_chunks(question.prompt)
        field_size = len(field_name) + len(prompt_chunks[0])
        field_size += sum(1 + len(chunk) for chunk in prompt_chunks[1:])
        if (
            embed is None
            or questions_on_page >= _PREVIEW_QUESTIONS_PER_PAGE
            or len(embed) + field_size > _EMBED_CONTENT_BUDGET
            or len(embed.fields) + len(prompt_chunks) > 25
        ):
            if embed is not None:
                pages.append(embed)
            embed = discord.Embed(
                title=f"#{survey.survey_id} — {escape_discord_text(survey.title)}",
                description=f"Status: **{survey.status.value.title()}**",
                color=0x5865F2,
            )
            questions_on_page = 0

        for chunk_index, prompt_chunk in enumerate(prompt_chunks):
            embed.add_field(
                name=field_name if chunk_index == 0 else "\u200b",
                value=prompt_chunk,
                inline=False,
            )
        questions_on_page += 1

    if embed is not None:
        pages.append(embed)

    page_count = len(pages)
    for page_index, embed in enumerate(pages, start=1):
        if page_count > 1:
            embed.set_footer(text=f"Page {page_index}/{page_count}")
    return pages


def _submitted_embed(session: SurveySession) -> discord.Embed:
    saved = (
        "Your response was saved."
        if session.survey.target_type is SurveyTargetType.MEMBER
        else "Your response was saved anonymously."
    )
    return discord.Embed(
        title="Survey submitted",
        description=(
            f"Thanks for completing **{escape_discord_text(session.survey.title)}**. "
            f"{saved}"
        ),
        color=0x57F287,
    )


def _closed_embed(session: SurveySession) -> discord.Embed:
    return discord.Embed(
        title="Survey closed",
        description=(
            f"**{escape_discord_text(session.survey.title)}** is no longer "
            "accepting responses."
        ),
        color=0x747F8D,
    )


def _delivery_nonce(recipient_id: int) -> str:
    """Stable send-time nonce so Discord deduplicates a rapidly retried send."""
    return f"cama-s:{recipient_id:x}"


def _message_matches_delivery(message, survey_id: int) -> bool:
    """Recognize a prior survey DM by its persistent component custom IDs.

    History-fetched messages never carry the send-time nonce (Discord only
    returns it on the send response and gateway create event), so the durable
    receipt marker is the ``survey:{survey_id}:`` custom_id prefix that every
    delivered survey DM's view components carry.
    """
    custom_id_prefix = f"survey:{survey_id}:"
    for action_row in getattr(message, "components", ()):
        for component in getattr(action_row, "children", ()):
            custom_id = getattr(component, "custom_id", None)
            if isinstance(custom_id, str) and custom_id.startswith(custom_id_prefix):
                return True
    return False


class SurveyTextModal(discord.ui.Modal):
    """Private text editor for one survey question."""

    def __init__(
        self,
        cog: SurveyCommands,
        session: SurveySession,
        question: SurveyQuestion,
    ) -> None:
        super().__init__(title=f"Question {question.position}", timeout=900)
        self.cog = cog
        self.session = session
        self.question = question
        existing = next(
            (
                answer.text_value
                for answer in session.answers
                if answer.question_id == question.question_id
                and not answer.skipped
                and answer.text_value is not None
            ),
            None,
        )
        self.response_text = discord.ui.TextInput(
            label="Your answer",
            style=discord.TextStyle.paragraph,
            required=True,
            max_length=MAX_TEXT_ANSWER_LENGTH,
            default=existing,
        )
        self.add_item(self.response_text)

    async def on_submit(self, interaction: discord.Interaction) -> None:
        await self.cog.answer_question(
            interaction,
            self.session.survey.survey_id,
            self.question.question_id,
            str(self.response_text.value),
        )

    async def on_error(
        self,
        interaction: discord.Interaction,
        error: Exception,
    ) -> None:
        logger.error("Survey text modal failed (%s)", type(error).__name__)
        await self.cog.respond_error(interaction, "Couldn't save that answer right now.")


class SurveyScoreSelect(discord.ui.Select):
    def __init__(
        self,
        cog: SurveyCommands,
        session: SurveySession,
        question: SurveyQuestion,
    ) -> None:
        self.cog = cog
        self.session = session
        self.question = question
        if question.question_type is SurveyQuestionType.NPS:
            values = range(0, 11)
            placeholder = "Choose 0–10"
        else:
            values = range(1, 6)
            placeholder = "Choose 1–5"
        super().__init__(
            placeholder=placeholder,
            min_values=1,
            max_values=1,
            options=[discord.SelectOption(label=str(value), value=str(value)) for value in values],
            custom_id=(
                f"survey:{session.survey.survey_id}:"
                f"question:{question.question_id}:score"
            ),
        )

    async def callback(self, interaction: discord.Interaction) -> None:
        await self.cog.answer_question(
            interaction,
            self.session.survey.survey_id,
            self.question.question_id,
            int(self.values[0]),
        )


class SurveyTextButton(discord.ui.Button):
    def __init__(
        self,
        cog: SurveyCommands,
        session: SurveySession,
        question: SurveyQuestion,
    ) -> None:
        super().__init__(
            label="Write answer",
            style=discord.ButtonStyle.primary,
            custom_id=(
                f"survey:{session.survey.survey_id}:"
                f"question:{question.question_id}:text"
            ),
        )
        self.cog = cog
        self.session = session
        self.question = question

    async def callback(self, interaction: discord.Interaction) -> None:
        await interaction.response.send_modal(
            SurveyTextModal(self.cog, self.session, self.question)
        )


class SurveySkipButton(discord.ui.Button):
    def __init__(
        self,
        cog: SurveyCommands,
        session: SurveySession,
        question: SurveyQuestion,
    ) -> None:
        super().__init__(
            label="Skip",
            style=discord.ButtonStyle.secondary,
            custom_id=(
                f"survey:{session.survey.survey_id}:"
                f"question:{question.question_id}:skip"
            ),
        )
        self.cog = cog
        self.session = session
        self.question = question

    async def callback(self, interaction: discord.Interaction) -> None:
        await self.cog.skip_question(
            interaction,
            self.session.survey.survey_id,
            self.question.question_id,
        )


class SurveyReturnToReviewButton(discord.ui.Button):
    def __init__(self, cog: SurveyCommands, session: SurveySession) -> None:
        super().__init__(
            label="Back to review",
            style=discord.ButtonStyle.secondary,
            custom_id=f"survey:{session.survey.survey_id}:review:return",
        )
        self.cog = cog
        self.session = session

    async def callback(self, interaction: discord.Interaction) -> None:
        await self.cog.return_to_review(interaction, self.session.survey.survey_id)


class SurveyQuestionView(discord.ui.View):
    """Persistent controls for the respondent's current question."""

    def __init__(
        self,
        cog: SurveyCommands,
        session: SurveySession,
        question: SurveyQuestion,
    ) -> None:
        super().__init__(timeout=None)
        self.cog = cog
        self.session = session
        self.question = question
        if question.question_type is SurveyQuestionType.TEXT:
            self.add_item(SurveyTextButton(cog, session, question))
        else:
            self.add_item(SurveyScoreSelect(cog, session, question))
        if not question.required:
            self.add_item(SurveySkipButton(cog, session, question))
        if session.recipient.in_review:
            self.add_item(SurveyReturnToReviewButton(cog, session))

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id == self.session.recipient.discord_id:
            return True
        await self.cog.respond_error(interaction, "This survey response isn't yours.")
        return False


class SurveyEditSelect(discord.ui.Select):
    def __init__(self, cog: SurveyCommands, session: SurveySession) -> None:
        options = [
            discord.SelectOption(
                label=f"Question {question.position}",
                description=truncate_field(
                    escape_discord_text(question.prompt.replace("\n", " ")),
                    90,
                ),
                value=str(question.question_id),
            )
            for question in session.questions
        ]
        super().__init__(
            placeholder="Edit an answer",
            min_values=1,
            max_values=1,
            options=options,
            custom_id=f"survey:{session.survey.survey_id}:review:edit",
        )
        self.cog = cog
        self.session = session

    async def callback(self, interaction: discord.Interaction) -> None:
        await self.cog.edit_response_question(
            interaction,
            self.session.survey.survey_id,
            int(self.values[0]),
        )


class SurveySubmitButton(discord.ui.Button):
    def __init__(self, cog: SurveyCommands, session: SurveySession) -> None:
        super().__init__(
            label="Submit response",
            style=discord.ButtonStyle.success,
            custom_id=f"survey:{session.survey.survey_id}:review:submit",
        )
        self.cog = cog
        self.session = session

    async def callback(self, interaction: discord.Interaction) -> None:
        await self.cog.submit_response(interaction, self.session.survey.survey_id)


class SurveyReviewView(discord.ui.View):
    """Persistent final review controls."""

    def __init__(self, cog: SurveyCommands, session: SurveySession) -> None:
        super().__init__(timeout=None)
        self.cog = cog
        self.session = session
        self.add_item(SurveyEditSelect(cog, session))
        self.add_item(SurveySubmitButton(cog, session))

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id == self.session.recipient.discord_id:
            return True
        await self.cog.respond_error(interaction, "This survey response isn't yours.")
        return False


class SurveyResultsView(discord.ui.View):
    """Requester-locked pagination for ephemeral admin reports."""

    def __init__(
        self,
        owner_id: int,
        pages: list[discord.Embed],
        interaction: discord.Interaction | None = None,
    ) -> None:
        super().__init__(timeout=_RESULTS_TIMEOUT_SECONDS)
        self.owner_id = owner_id
        self.pages = pages
        self.index = 0
        self._interaction = interaction
        self._sync_buttons()

    def _sync_buttons(self) -> None:
        self.previous.disabled = self.index == 0
        self.next.disabled = self.index >= len(self.pages) - 1

    async def on_timeout(self) -> None:
        """Disable the dead buttons so the stale report cannot look clickable."""
        for child in self.children:
            child.disabled = True
        if self._interaction is None:
            return
        try:
            # Every click refreshes both the view timeout and the tracked
            # interaction, so the 600s timeout always fires inside the last
            # token's 15-minute window and the ephemeral report stays editable.
            await self._interaction.edit_original_response(view=self)
        except Exception as exc:
            logger.debug(
                "Survey results view could not be disabled after timeout (%s)",
                type(exc).__name__,
            )

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id == self.owner_id:
            return True
        await interaction.response.send_message(
            "Only the admin who opened this report can page through it.",
            ephemeral=True,
        )
        return False

    @discord.ui.button(label="Previous", style=discord.ButtonStyle.secondary)
    async def previous(
        self,
        interaction: discord.Interaction,
        _button: discord.ui.Button,
    ) -> None:
        self.index = max(0, self.index - 1)
        self._sync_buttons()
        await interaction.response.edit_message(
            embed=self.pages[self.index],
            view=self,
            allowed_mentions=_ALLOWED_MENTIONS,
        )
        # discord.py pushes the view timeout forward on every click; keep the
        # freshest token so the on_timeout edit stays inside its 15m window.
        # Only after the edit succeeds — a failed click's unacknowledged token
        # cannot perform the timeout edit, while the previous one still can.
        self._interaction = interaction

    @discord.ui.button(label="Next", style=discord.ButtonStyle.secondary)
    async def next(
        self,
        interaction: discord.Interaction,
        _button: discord.ui.Button,
    ) -> None:
        self.index = min(len(self.pages) - 1, self.index + 1)
        self._sync_buttons()
        await interaction.response.edit_message(
            embed=self.pages[self.index],
            view=self,
            allowed_mentions=_ALLOWED_MENTIONS,
        )
        # See previous(): only adopt the token once the edit succeeded.
        self._interaction = interaction


def _percent(value: float) -> str:
    return f"{value * 100:.1f}%"


def _escaped_field_chunks(value: str, limit: int = 1_024) -> list[str]:
    """Escape all text while keeping every field chunk within Discord's limit."""
    chunks: list[str] = []
    current: list[str] = []
    current_length = 0
    for character in value:
        escaped = escape_discord_text(character)
        if current and current_length + len(escaped) > limit:
            chunks.append("".join(current))
            current = []
            current_length = 0
        current.append(escaped)
        current_length += len(escaped)
    if current:
        chunks.append("".join(current))
    return chunks or ["\u200b"]


def _results_pages(results: SurveyResults) -> list[discord.Embed]:
    """Build admin-safe pages from aggregate DTOs only."""
    overview = discord.Embed(
        title=f"Survey results — {escape_discord_text(results.title)}",
        color=0x5865F2,
    )
    overview.add_field(name="Status", value=results.status.value.title(), inline=True)
    overview.add_field(
        name="Delivery",
        value=(
            f"{results.delivered_count}/{results.recipient_count} "
            f"({_percent(results.delivery_rate)})"
        ),
        inline=True,
    )
    overview.add_field(
        name="Completed",
        value=(
            f"{results.completed_count}/{results.delivered_count} delivered "
            f"({_percent(results.response_rate)})"
        ),
        inline=True,
    )
    overview.add_field(
        name="Started",
        value=(
            f"{results.started_count}/{results.delivered_count} delivered "
            f"({_percent(results.started_rate)})"
        ),
        inline=True,
    )
    overview.add_field(
        name="Completion after starting",
        value=_percent(results.completion_rate),
        inline=True,
    )
    overview.set_footer(text="Only submitted responses are included below.")
    pages = [overview]

    for question in results.questions:
        prompt = escape_discord_text(question.prompt)
        if question.question_type is SurveyQuestionType.NPS:
            embed = discord.Embed(
                title=f"Question {question.position} — NPS",
                description=prompt,
                color=0x5865F2,
            )
            embed.add_field(
                name="NPS",
                value=str(question.nps_score) if question.nps_score is not None else "N/A",
                inline=True,
            )
            embed.add_field(
                name="Answered / skipped",
                value=f"{question.response_count} / {question.skipped_count}",
                inline=True,
            )
            embed.add_field(
                name="Promoters (9–10)", value=str(question.promoters), inline=True
            )
            embed.add_field(
                name="Passives (7–8)", value=str(question.passives), inline=True
            )
            embed.add_field(
                name="Detractors (0–6)", value=str(question.detractors), inline=True
            )
            pages.append(embed)
            continue

        if question.question_type is SurveyQuestionType.RATING:
            embed = discord.Embed(
                title=f"Question {question.position} — Rating",
                description=prompt,
                color=0x5865F2,
            )
            average = (
                f"{question.rating_average:.2f}"
                if question.rating_average is not None
                else "N/A"
            )
            embed.add_field(name="Average", value=average, inline=True)
            embed.add_field(
                name="Answered / skipped",
                value=f"{question.response_count} / {question.skipped_count}",
                inline=True,
            )
            embed.add_field(
                name="Distribution",
                value="\n".join(
                    f"{score}: {count}"
                    for score, count in enumerate(question.rating_distribution, start=1)
                ),
                inline=False,
            )
            pages.append(embed)
            continue

        responses = list(question.text_responses)
        if not responses:
            embed = discord.Embed(
                title=f"Question {question.position} — Written responses",
                description=prompt,
                color=0x5865F2,
            )
            embed.add_field(
                name="Responses",
                value=f"No written responses • {question.skipped_count} skipped",
                inline=False,
            )
            pages.append(embed)
            continue

        embed: discord.Embed | None = None
        responses_on_page = 0
        for response_number, response in enumerate(responses, start=1):
            field_chunks = _escaped_field_chunks(response)
            field_size = len(f"Anonymous response {response_number}") + len(
                field_chunks[0]
            )
            field_size += sum(1 + len(chunk) for chunk in field_chunks[1:])
            if (
                embed is None
                or responses_on_page >= _TEXT_RESULTS_PER_PAGE
                or len(embed) + field_size > _EMBED_CONTENT_BUDGET
                or len(embed.fields) + len(field_chunks) > 25
            ):
                if embed is not None:
                    embed.set_footer(
                        text=(
                            f"{question.response_count} answered • "
                            f"{question.skipped_count} skipped"
                        )
                    )
                    pages.append(embed)
                embed = discord.Embed(
                    title=f"Question {question.position} — Written responses",
                    description=prompt,
                    color=0x5865F2,
                )
                responses_on_page = 0

            for chunk_index, field_chunk in enumerate(field_chunks):
                embed.add_field(
                    name=(
                        f"Anonymous response {response_number}"
                        if chunk_index == 0
                        else "\u200b"
                    ),
                    value=field_chunk,
                    inline=False,
                )
            responses_on_page += 1

        if embed is not None:
            embed.set_footer(
                text=(
                    f"{question.response_count} answered • "
                    f"{question.skipped_count} skipped"
                )
            )
            pages.append(embed)
    return pages


def _response_state_fingerprint(session: SurveySession, message_id: int) -> str:
    """Digest every rendered input of a respondent DM without retaining it.

    Hashing keeps respondents' free-text answers out of long-lived process
    memory while still detecting any state change that would re-render.
    """
    state = (
        message_id,
        session.survey.status.value,
        session.recipient.updated_at,
        session.recipient.current_question_id,
        session.recipient.in_review,
        session.recipient.submitted_at,
        session.answers,
    )
    return hashlib.sha256(repr(state).encode("utf-8")).hexdigest()


class SurveyCommands(commands.Cog):
    """Admin commands plus persistent respondent controls for surveys."""

    survey = app_commands.Group(
        name="survey",
        description="Create and manage private one-off surveys",
        guild_only=True,
    )
    question = app_commands.Group(
        name="question",
        description="Edit draft survey questions",
        parent=survey,
    )

    def __init__(self, bot: commands.Bot, survey_service, player_service) -> None:
        self.bot = bot
        self.survey_service = survey_service
        self.player_service = player_service
        self._delivery_lock = asyncio.Lock()
        self._delivery_semaphore = asyncio.Semaphore(_DELIVERY_CONCURRENCY)
        self._recovery_edit_semaphore = asyncio.Semaphore(_DELIVERY_CONCURRENCY)
        self._ready_recovery_lock = asyncio.Lock()
        self._delivery_recovery_handle: asyncio.TimerHandle | None = None
        self._delivery_recovery_task: asyncio.Task[None] | None = None
        self._send_dispatch_tasks: set[asyncio.Task[None]] = set()
        # Digest of the last (message_id, survey status, recipient/answer
        # state) this process successfully wrote to each respondent DM, so
        # reconnect reconciliation can skip edits that would re-render
        # identical content. Stored hashed — never raw answer content — and
        # evicted once the response UI is finalized.
        self._reconciled_response_fingerprints: dict[tuple[int, int], str] = {}
        self._unloaded = False
        self._response_locks: weakref.WeakValueDictionary[
            tuple[int, int], asyncio.Lock
        ] = weakref.WeakValueDictionary()

    def _response_lock(self, survey_id: int, discord_id: int) -> asyncio.Lock:
        return self._response_locks.setdefault((survey_id, discord_id), asyncio.Lock())

    def _schedule_delivery_recovery(self, delay_seconds: int) -> None:
        """Schedule a non-blocking retry after receipt/lease visibility settles."""
        if self._unloaded:
            return
        loop = asyncio.get_running_loop()
        delay = max(1, delay_seconds)
        deadline = loop.time() + delay
        existing = self._delivery_recovery_handle
        if (
            existing is not None
            and not existing.cancelled()
            and existing.when() <= deadline
        ):
            return
        if existing is not None:
            existing.cancel()
        self._delivery_recovery_handle = loop.call_later(
            delay,
            self._start_scheduled_delivery_recovery,
        )

    def _start_scheduled_delivery_recovery(self) -> None:
        self._delivery_recovery_handle = None
        if self._unloaded:
            return
        active = self._delivery_recovery_task
        if active is not None and not active.done():
            self._schedule_delivery_recovery(1)
            return
        task = asyncio.create_task(self._run_scheduled_delivery_recovery())
        self._delivery_recovery_task = task
        task.add_done_callback(self._clear_delivery_recovery_task)

    def _clear_delivery_recovery_task(self, task: asyncio.Task[None]) -> None:
        if self._delivery_recovery_task is task:
            self._delivery_recovery_task = None

    async def cog_unload(self) -> None:
        """Stop timers and workers owned by this cog during extension reload."""
        self._unloaded = True
        handle = self._delivery_recovery_handle
        self._delivery_recovery_handle = None
        if handle is not None:
            handle.cancel()
        task = self._delivery_recovery_task
        self._delivery_recovery_task = None
        if task is not None and not task.done():
            task.cancel()
        for dispatch_task in list(self._send_dispatch_tasks):
            dispatch_task.cancel()
        self._send_dispatch_tasks.clear()

    async def _run_scheduled_delivery_recovery(self) -> None:
        try:
            await self.dispatch_pending()
            await self.reconcile_response_messages()
            await self._schedule_if_unconfirmed_deliveries()
        except Exception as exc:
            logger.error(
                "Scheduled survey delivery recovery failed (%s)",
                type(exc).__name__,
            )
            self._schedule_delivery_recovery(_DELIVERY_STALE_SECONDS)

    async def _schedule_if_unconfirmed_deliveries(self) -> None:
        remaining = await asyncio.to_thread(
            self.survey_service.list_unconfirmed_deliveries,
            None,
            None,
            0,
            0,
        )
        if remaining:
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)

    async def respond_error(
        self,
        interaction: discord.Interaction,
        message: str,
    ) -> None:
        content = f"❌ {message}"
        try:
            if interaction.response.is_done():
                await interaction.followup.send(
                    content,
                    ephemeral=True,
                    allowed_mentions=_ALLOWED_MENTIONS,
                )
            else:
                await interaction.response.send_message(
                    content,
                    ephemeral=True,
                    allowed_mentions=_ALLOWED_MENTIONS,
                )
        except Exception as exc:
            # An expired/consumed interaction token must not raise out of the
            # error reporter itself and mask what the caller already did.
            logger.warning(
                "Survey error response could not be delivered (%s)",
                type(exc).__name__,
            )

    async def _require_admin(self, interaction: discord.Interaction) -> bool:
        if has_admin_permission(interaction):
            return True
        await self.respond_error(
            interaction,
            "Admin only. You need Administrator or Manage Server permissions.",
        )
        return False

    async def _edit_response_message(
        self,
        interaction: discord.Interaction,
        session: SurveySession,
    ) -> None:
        if session.recipient.submitted_at is not None:
            embed = _submitted_embed(session)
            view = None
        elif session.recipient.in_review and session.recipient.current_question_id is None:
            embed = _review_embed(session)
            view = SurveyReviewView(self, session)
        else:
            question = session.current_question
            if question is None:
                await self.respond_error(interaction, "Couldn't restore the current question.")
                return
            embed = _question_embed(session, question)
            view = SurveyQuestionView(self, session, question)
        try:
            await interaction.edit_original_response(
                embed=embed,
                view=view,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except Exception:
            # The response mutation is already durable. Reconcile the stored
            # DM later if Discord could not apply the matching UI state now.
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
            raise
        if session.recipient.submitted_at is not None:
            await self._finalize_response_ui(session)

    async def _finalize_response_ui(self, session: SurveySession) -> None:
        message_id = session.recipient.ui_message_id or session.recipient.dm_message_id
        if message_id is None:
            return
        try:
            await asyncio.to_thread(
                self.survey_service.finalize_response_ui,
                session.survey.survey_id,
                session.recipient.discord_id,
                message_id,
            )
            # The session left the recoverable set for good, so its
            # reconciliation fingerprint can never be consulted again.
            self._reconciled_response_fingerprints.pop(
                (session.survey.survey_id, session.recipient.discord_id),
                None,
            )
        except Exception as exc:
            # The final, viewless Discord message is already correct. Leaving
            # this unfinalized lets reconnect recovery safely try again.
            logger.warning(
                "Survey final UI state could not be persisted (%s)",
                type(exc).__name__,
            )
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)

    async def answer_question(
        self,
        interaction: discord.Interaction,
        survey_id: int,
        question_id: int,
        value: int | str,
    ) -> None:
        if not await safe_defer(interaction):
            return
        async with self._response_lock(survey_id, interaction.user.id):
            try:
                session = await asyncio.to_thread(
                    self.survey_service.answer_question,
                    survey_id,
                    interaction.user.id,
                    question_id,
                    value,
                )
            except ValueError as exc:
                await self.respond_error(interaction, str(exc))
                return
            except Exception as exc:
                logger.error("Survey answer handling failed (%s)", type(exc).__name__)
                await self.respond_error(interaction, "Couldn't save that answer right now.")
                return
            try:
                await self._edit_response_message(interaction, session)
            except Exception as exc:
                # The answer is durably saved; only the DM re-render failed and
                # recovery is already scheduled. Never report a save failure.
                logger.warning(
                    "Survey answer saved but the DM refresh failed (%s)",
                    type(exc).__name__,
                )

    async def skip_question(
        self,
        interaction: discord.Interaction,
        survey_id: int,
        question_id: int,
    ) -> None:
        if not await safe_defer(interaction):
            return
        async with self._response_lock(survey_id, interaction.user.id):
            try:
                session = await asyncio.to_thread(
                    self.survey_service.skip_question,
                    survey_id,
                    interaction.user.id,
                    question_id,
                )
            except ValueError as exc:
                await self.respond_error(interaction, str(exc))
                return
            except Exception as exc:
                logger.error("Survey skip handling failed (%s)", type(exc).__name__)
                await self.respond_error(interaction, "Couldn't skip that question right now.")
                return
            try:
                await self._edit_response_message(interaction, session)
            except Exception as exc:
                # The skip is durably saved; only the DM re-render failed and
                # recovery is already scheduled. Never report a skip failure.
                logger.warning(
                    "Survey skip saved but the DM refresh failed (%s)",
                    type(exc).__name__,
                )

    async def edit_response_question(
        self,
        interaction: discord.Interaction,
        survey_id: int,
        question_id: int,
    ) -> None:
        if not await safe_defer(interaction):
            return
        async with self._response_lock(survey_id, interaction.user.id):
            try:
                session = await asyncio.to_thread(
                    self.survey_service.select_response_question,
                    survey_id,
                    interaction.user.id,
                    question_id,
                )
            except ValueError as exc:
                await self.respond_error(interaction, str(exc))
                return
            except Exception as exc:
                logger.error(
                    "Survey review edit selection failed (%s)", type(exc).__name__
                )
                await self.respond_error(interaction, "Couldn't open that answer right now.")
                return
            try:
                await self._edit_response_message(interaction, session)
            except Exception as exc:
                # The selection is durably saved; only the DM re-render failed
                # and recovery is already scheduled. Never report a failure.
                logger.warning(
                    "Survey edit selection saved but the DM refresh failed (%s)",
                    type(exc).__name__,
                )

    async def return_to_review(
        self,
        interaction: discord.Interaction,
        survey_id: int,
    ) -> None:
        if not await safe_defer(interaction):
            return
        async with self._response_lock(survey_id, interaction.user.id):
            try:
                session = await asyncio.to_thread(
                    self.survey_service.begin_review,
                    survey_id,
                    interaction.user.id,
                )
            except ValueError as exc:
                await self.respond_error(interaction, str(exc))
                return
            except Exception as exc:
                logger.error("Survey return-to-review failed (%s)", type(exc).__name__)
                await self.respond_error(interaction, "Couldn't return to review right now.")
                return
            try:
                await self._edit_response_message(interaction, session)
            except Exception as exc:
                # The review state is durably saved; only the DM re-render
                # failed and recovery is already scheduled.
                logger.warning(
                    "Survey review opened but the DM refresh failed (%s)",
                    type(exc).__name__,
                )

    async def submit_response(
        self,
        interaction: discord.Interaction,
        survey_id: int,
    ) -> None:
        if not await safe_defer(interaction):
            return
        async with self._response_lock(survey_id, interaction.user.id):
            try:
                submitted = await asyncio.to_thread(
                    self.survey_service.submit_response,
                    survey_id,
                    interaction.user.id,
                )
                session = await asyncio.to_thread(
                    self.survey_service.get_response_session,
                    survey_id,
                    interaction.user.id,
                )
                if session is None:
                    raise ValueError("This survey response no longer exists")
                if not submitted and session.recipient.submitted_at is None:
                    raise ValueError("This response could not be submitted")
            except ValueError as exc:
                await self.respond_error(interaction, str(exc))
                return
            except Exception as exc:
                logger.error("Survey submission failed (%s)", type(exc).__name__)
                await self.respond_error(interaction, "Couldn't submit that response right now.")
                return
            try:
                await self._edit_response_message(interaction, session)
            except Exception as exc:
                # The submission is durably recorded and already counted in
                # results; only the DM re-render failed and recovery is
                # already scheduled. Never report a submit failure.
                logger.warning(
                    "Survey response submitted but the DM refresh failed (%s)",
                    type(exc).__name__,
                )

    def _response_view(self, session: SurveySession) -> discord.ui.View | None:
        if (
            session.recipient.submitted_at is not None
            or session.survey.status is SurveyStatus.CLOSED
        ):
            return None
        if session.recipient.in_review and session.recipient.current_question_id is None:
            return SurveyReviewView(self, session)
        question = session.current_question
        if question is None:
            return None
        return SurveyQuestionView(self, session, question)

    def _response_embed(self, session: SurveySession) -> discord.Embed:
        if session.recipient.submitted_at is not None:
            return _submitted_embed(session)
        if session.survey.status is SurveyStatus.CLOSED:
            return _closed_embed(session)
        if session.recipient.in_review and session.recipient.current_question_id is None:
            return _review_embed(session)
        question = session.current_question
        if question is None:
            raise ValueError("Survey response has no current question")
        return _question_embed(session, question)

    async def _deliver_recipient(self, recipient) -> None:
        async with self._delivery_semaphore:
            try:
                session = await asyncio.to_thread(
                    self.survey_service.get_response_session,
                    recipient.survey_id,
                    recipient.discord_id,
                )
            except Exception as exc:
                # No Discord send was attempted, so this attempt can safely
                # become a retryable delivery failure.
                error_name = type(exc).__name__
                logger.warning(
                    "Survey delivery session could not be loaded (%s)",
                    error_name,
                )
                try:
                    await asyncio.to_thread(
                        self.survey_service.mark_delivery_failed,
                        recipient.recipient_id,
                        recipient.attempt_count,
                        error_name,
                    )
                except Exception as persist_exc:
                    logger.warning(
                        "Survey delivery failure state could not be saved (%s)",
                        type(persist_exc).__name__,
                    )
                self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
                return

            if session is None:
                error_name = "SessionUnavailable"
                logger.warning("Survey DM delivery failed (%s)", error_name)
                try:
                    await asyncio.to_thread(
                        self.survey_service.mark_delivery_failed,
                        recipient.recipient_id,
                        recipient.attempt_count,
                        error_name,
                    )
                except Exception as exc:
                    logger.warning(
                        "Survey delivery failure state could not be saved (%s)",
                        type(exc).__name__,
                    )
                self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
                return

            try:
                user = self.bot.get_user(recipient.discord_id)
                if user is None:
                    user = await self.bot.fetch_user(recipient.discord_id)
                await asyncio.to_thread(
                    self.survey_service.renew_delivery_claim,
                    recipient.recipient_id,
                    recipient.attempt_count,
                )
                view = self._response_view(session)
                message = await user.send(
                    embed=self._response_embed(session),
                    view=view,
                    nonce=_delivery_nonce(recipient.recipient_id),
                    allowed_mentions=_ALLOWED_MENTIONS,
                )
            except Exception as exc:
                # Never log respondent IDs or answer content. The persisted error is
                # intentionally reduced to a category for the same privacy reason.
                error_name = type(exc).__name__
                logger.warning("Survey DM delivery failed (%s)", error_name)
                try:
                    await asyncio.to_thread(
                        self.survey_service.mark_delivery_failed,
                        recipient.recipient_id,
                        recipient.attempt_count,
                        error_name,
                    )
                except Exception as persist_exc:
                    logger.warning(
                        "Survey delivery failure state could not be saved (%s)",
                        type(persist_exc).__name__,
                    )
                self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
                return

            try:
                await asyncio.to_thread(
                    self.survey_service.mark_delivery_sent,
                    recipient.recipient_id,
                    recipient.attempt_count,
                    int(message.channel.id),
                    int(message.id),
                )
            except Exception as exc:
                # The DM succeeded. Do not relabel it as failed: the message's
                # persistent component IDs and exact receipt let recovery
                # resolve it.
                logger.warning(
                    "Survey delivery receipt could not be persisted (%s)",
                    type(exc).__name__,
                )
                try:
                    await asyncio.to_thread(
                        self.survey_service.reconcile_delivery_sent,
                        recipient.recipient_id,
                        recipient.attempt_count,
                        int(message.channel.id),
                        int(message.id),
                    )
                    latest = await asyncio.to_thread(
                        self.survey_service.get_response_session,
                        recipient.survey_id,
                        recipient.discord_id,
                    )
                    if latest is not None and latest.survey.status is SurveyStatus.CLOSED:
                        await self._reconcile_response_message(latest)
                except Exception as reconcile_exc:
                    logger.warning(
                        "Survey delivery receipt recovery was deferred (%s)",
                        type(reconcile_exc).__name__,
                    )
                    self._schedule_delivery_recovery(_DELIVERY_STALE_SECONDS)

    async def _reconcile_delivery_receipt(self, recipient) -> bool:
        """Check DM history before a possibly delivered attempt can be retried."""
        async with self._delivery_semaphore:
            try:
                user = self.bot.get_user(recipient.discord_id)
                if user is None:
                    user = await self.bot.fetch_user(recipient.discord_id)
                channel = await user.create_dm()
                # Search from recipient snapshot time, not only the latest
                # claim, so that a receipt from an earlier attempt is found.
                after = datetime.fromtimestamp(
                    max(0, recipient.created_at - 30),
                    tz=UTC,
                )
                bot_user_id = getattr(getattr(self.bot, "user", None), "id", None)
                matched_message = None
                # Scan newest-first with a bound: a delivered survey DM sits
                # among the bot's recent messages to that user, and an
                # unbounded walk of a busy DM channel is paginated REST work
                # done once per unconfirmed attempt.
                async for candidate in channel.history(
                    limit=_DELIVERY_RECEIPT_SCAN_LIMIT,
                    after=after,
                    oldest_first=False,
                ):
                    author_id = getattr(getattr(candidate, "author", None), "id", None)
                    if (
                        bot_user_id is not None
                        and author_id is not None
                        and author_id != bot_user_id
                    ):
                        continue
                    if _message_matches_delivery(candidate, recipient.survey_id):
                        matched_message = candidate
                        break

                if matched_message is not None:
                    message_channel = getattr(matched_message, "channel", channel)
                    await asyncio.to_thread(
                        self.survey_service.reconcile_delivery_sent,
                        recipient.recipient_id,
                        recipient.attempt_count,
                        int(message_channel.id),
                        int(matched_message.id),
                    )
                else:
                    await asyncio.to_thread(
                        self.survey_service.mark_delivery_receipt_checked,
                        recipient.recipient_id,
                        recipient.attempt_count,
                        recipient.claimed_at,
                        recipient.delivery_status,
                        recipient.updated_at,
                    )
                return True
            except Exception as exc:
                # An unchecked attempt must remain ineligible for requeue. A
                # later dispatch/reconnect will retry this history inspection.
                logger.warning(
                    "Survey delivery receipt reconciliation failed (%s)",
                    type(exc).__name__,
                )
                return False

    async def reconcile_unconfirmed_delivery_receipts(
        self,
        guild_id: int | None = None,
        survey_id: int | None = None,
        sending_stale_after_seconds: int = _DELIVERY_STALE_SECONDS,
        failed_grace_seconds: int = _DELIVERY_RECEIPT_GRACE_SECONDS,
    ) -> int:
        recipients = await asyncio.to_thread(
            self.survey_service.list_unconfirmed_deliveries,
            guild_id,
            survey_id,
            sending_stale_after_seconds,
            failed_grace_seconds,
        )
        if not recipients:
            return 0
        outcomes = await asyncio.gather(
            *(self._reconcile_delivery_receipt(recipient) for recipient in recipients)
        )
        return sum(outcomes)

    async def _dispatch_pending_locked(
        self,
        guild_id: int | None = None,
        survey_id: int | None = None,
    ) -> int:
        """Drain pending deliveries while ``_delivery_lock`` is held.

        ``guild_id``/``survey_id`` only scope the per-iteration receipt
        reconciliation (the claim queue is global by design); pass them when
        dispatching on behalf of one campaign so its loop does not run
        bot-wide DM-history scans.
        """
        try:
            delivered = 0
            while True:
                await self.reconcile_unconfirmed_delivery_receipts(guild_id, survey_id)
                await asyncio.to_thread(
                    self.survey_service.queue_requested_delivery_retries,
                )
                await asyncio.to_thread(
                    self.survey_service.recover_stale_deliveries,
                    _DELIVERY_STALE_SECONDS,
                )
                claims = await asyncio.to_thread(
                    self.survey_service.claim_deliveries,
                    _DELIVERY_BATCH_SIZE,
                    _DELIVERY_STALE_SECONDS,
                )
                if not claims:
                    break
                await asyncio.gather(
                    *(self._deliver_recipient(recipient) for recipient in claims),
                    return_exceptions=False,
                )
                delivered += len(claims)
            return delivered
        except Exception:
            # A campaign may already be open or have a durable retry intent.
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
            raise

    async def dispatch_pending(self) -> int:
        """Drain durable pending deliveries without concurrent duplicate workers."""
        async with self._delivery_lock:
            return await self._dispatch_pending_locked()

    def _start_send_dispatch(self, guild_id: int, survey_id: int) -> None:
        """Deliver an opened campaign in the background, off the command token.

        The recipients are already durably pending, so the admin gets an
        immediate response while the claim/receipt machinery (shared with
        scheduled recovery) delivers the DMs. A strong task reference is kept
        so the dispatch cannot be garbage-collected mid-campaign.
        """
        task = asyncio.create_task(self._run_send_dispatch(guild_id, survey_id))
        self._send_dispatch_tasks.add(task)
        task.add_done_callback(self._send_dispatch_tasks.discard)

    async def _run_send_dispatch(self, guild_id: int, survey_id: int) -> None:
        try:
            async with self._delivery_lock:
                await self._dispatch_pending_locked(guild_id, survey_id)
            await self._schedule_if_unconfirmed_deliveries()
        except Exception as exc:
            logger.error(
                "Survey send delivery dispatch failed (%s)",
                type(exc).__name__,
            )
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)

    async def restore_response_views(self) -> int:
        sessions = await asyncio.to_thread(
            self.survey_service.list_recoverable_response_sessions
        )
        restored = 0
        for session in sessions:
            message_id = session.recipient.ui_message_id or session.recipient.dm_message_id
            view = self._response_view(session)
            if message_id is None or view is None:
                continue
            self.bot.add_view(view, message_id=message_id)
            restored += 1
        return restored

    async def _reconcile_response_message(self, session: SurveySession) -> None:
        async with self._recovery_edit_semaphore:
            async with self._response_lock(
                session.survey.survey_id,
                session.recipient.discord_id,
            ):
                try:
                    latest = await asyncio.to_thread(
                        self.survey_service.get_response_session,
                        session.survey.survey_id,
                        session.recipient.discord_id,
                    )
                    if latest is None:
                        return
                    message_id = (
                        latest.recipient.ui_message_id or latest.recipient.dm_message_id
                    )
                    if message_id is None:
                        return
                    key = (latest.survey.survey_id, latest.recipient.discord_id)
                    fingerprint = _response_state_fingerprint(latest, message_id)
                    # Skip the Discord edit when this process already rendered
                    # exactly this state to this message (e.g. repeated
                    # on_ready reconnects with no respondent activity).
                    if self._reconciled_response_fingerprints.get(key) != fingerprint:
                        user = self.bot.get_user(latest.recipient.discord_id)
                        if user is None:
                            user = await self.bot.fetch_user(latest.recipient.discord_id)
                        channel = await user.create_dm()
                        message = channel.get_partial_message(message_id)
                        await message.edit(
                            embed=self._response_embed(latest),
                            view=self._response_view(latest),
                            allowed_mentions=_ALLOWED_MENTIONS,
                        )
                        self._reconciled_response_fingerprints[key] = fingerprint
                    if (
                        latest.recipient.submitted_at is not None
                        or latest.survey.status is SurveyStatus.CLOSED
                    ):
                        await self._finalize_response_ui(latest)
                except Exception as exc:
                    logger.warning(
                        "Survey DM state reconciliation failed (%s)",
                        type(exc).__name__,
                    )
                    self._schedule_delivery_recovery(
                        _DELIVERY_RECEIPT_GRACE_SECONDS
                    )

    async def reconcile_response_messages(self) -> int:
        """Make Discord's stored components match the last committed DB state."""
        sessions = await asyncio.to_thread(
            self.survey_service.list_recoverable_response_sessions
        )
        if not sessions:
            return 0
        await asyncio.gather(
            *(self._reconcile_response_message(session) for session in sessions)
        )
        return len(sessions)

    @commands.Cog.listener()
    async def on_ready(self) -> None:
        if self._ready_recovery_lock.locked():
            return
        async with self._ready_recovery_lock:
            try:
                # Resolve ambiguous Discord receipts before requeueing anything.
                # This preserves exactly-once delivery even after the short
                # Discord nonce-deduplication window has elapsed.
                async with self._delivery_lock:
                    await self._dispatch_pending_locked()
                await self.reconcile_response_messages()
                await self._schedule_if_unconfirmed_deliveries()
            except Exception as exc:
                logger.error(
                    "Survey pending-delivery recovery failed (%s)", type(exc).__name__
                )
                self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)

    async def _registered_member_ids(self, guild: discord.Guild) -> set[int]:
        players = await asyncio.to_thread(self.player_service.get_all, guild.id)
        registered_ids = {
            int(player.discord_id)
            for player in players
            if isinstance(getattr(player, "discord_id", None), int)
            and not isinstance(player.discord_id, bool)
            and player.discord_id > 0
        }
        return {
            member.id
            for member in guild.members
            if member.id in registered_ids and not member.bot
        }

    async def _resolve_recipients(
        self,
        guild: discord.Guild,
        target_type: SurveyTargetType,
        *,
        member: discord.Member | None,
        role: discord.Role | None,
        registered_only: bool,
    ) -> list[int]:
        current_members = {guild_member.id: guild_member for guild_member in guild.members}
        registered_ids: set[int] | None = None
        if target_type is SurveyTargetType.ALL_REGISTERED or registered_only:
            registered_ids = await self._registered_member_ids(guild)

        if target_type is SurveyTargetType.ALL_REGISTERED:
            if member is not None or role is not None:
                raise ValueError("All registered targeting does not accept a member or role")
            if registered_only:
                raise ValueError("All registered targeting is already registered-only")
            return sorted(registered_ids or set())

        if target_type is SurveyTargetType.MEMBER:
            if member is None or role is not None:
                raise ValueError("Choose exactly one member for member targeting")
            current_member = current_members.get(member.id)
            if member.bot or (current_member is not None and current_member.bot):
                raise ValueError("Bots cannot receive surveys")
            if member.guild.id != guild.id or current_member is None:
                raise ValueError("That member is not in this server")
            if registered_only and member.id not in (registered_ids or set()):
                raise ValueError("That member is not a registered Cama player")
            return [member.id]

        if role is None or member is not None:
            raise ValueError("Choose exactly one role for role targeting")
        if role.guild.id != guild.id:
            raise ValueError("That role is not in this server")
        recipient_ids = {
            role_member.id
            for role_member in role.members
            if role_member.id in current_members
            and not role_member.bot
            and not current_members[role_member.id].bot
            and (
                not registered_only
                or role_member.id in (registered_ids or set())
            )
        }
        return sorted(recipient_ids)

    @survey.command(name="create", description="Create a one-off survey draft")
    @app_commands.describe(title="Survey title shown to respondents")
    @require_guild
    async def create(
        self,
        interaction: discord.Interaction,
        title: app_commands.Range[str, 1, MAX_SURVEY_TITLE_LENGTH],
    ) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            survey = await asyncio.to_thread(
                self.survey_service.create_survey,
                interaction.guild.id,
                str(title),
            )
            await safe_followup(
                interaction,
                content=(
                    f"✅ Created survey **#{survey.survey_id} — "
                    f"{escape_discord_text(survey.title)}**. "
                    "Add questions before sending it."
                ),
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            await self.respond_error(interaction, str(exc))

    @survey.command(name="list", description="List this server's surveys")
    @app_commands.choices(status=STATUS_CHOICES)
    @require_guild
    async def list_surveys(
        self,
        interaction: discord.Interaction,
        status: app_commands.Choice[str] | None = None,
    ) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        surveys = await asyncio.to_thread(
            self.survey_service.list_surveys,
            interaction.guild.id,
            status.value if status else None,
        )
        if not surveys:
            await safe_followup(
                interaction,
                content="No surveys found.",
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
            return
        embed = discord.Embed(title="Surveys", color=0x5865F2)
        for survey in surveys[:20]:
            embed.add_field(
                name=f"#{survey.survey_id} • {survey.status.value.title()}",
                value=truncate_field(escape_discord_text(survey.title), 300),
                inline=False,
            )
        if len(surveys) > 20:
            embed.set_footer(text=f"Showing 20 of {len(surveys)} surveys")
        await safe_followup(
            interaction,
            embed=embed,
            ephemeral=True,
            allowed_mentions=_ALLOWED_MENTIONS,
        )

    @survey.command(name="preview", description="Preview a survey and its questions")
    @app_commands.describe(survey_id="Survey number")
    @require_guild
    async def preview(self, interaction: discord.Interaction, survey_id: int) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            survey, questions = await asyncio.gather(
                asyncio.to_thread(
                    self.survey_service.get_survey,
                    interaction.guild.id,
                    survey_id,
                ),
                asyncio.to_thread(
                    self.survey_service.get_questions,
                    interaction.guild.id,
                    survey_id,
                ),
            )
            if survey is None:
                raise ValueError("Survey not found in this server")
            pages = _preview_pages(survey, questions)
            view = (
                SurveyResultsView(interaction.user.id, pages, interaction)
                if len(pages) > 1
                else None
            )
            await safe_followup(
                interaction,
                embed=pages[0],
                view=view,
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            await self.respond_error(interaction, str(exc))

    @survey.command(name="delete", description="Delete a survey draft")
    @require_guild
    async def delete(self, interaction: discord.Interaction, survey_id: int) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            deleted = await asyncio.to_thread(
                self.survey_service.delete_survey,
                interaction.guild.id,
                survey_id,
            )
            if not deleted:
                raise ValueError("Survey not found in this server")
            await safe_followup(
                interaction,
                content=f"✅ Deleted survey **#{survey_id}**.",
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            await self.respond_error(interaction, str(exc))

    @question.command(name="add", description="Add a question to a survey draft")
    @app_commands.describe(
        survey_id="Survey number",
        question_type="Answer format",
        prompt="Question shown to respondents",
        required="Whether respondents must answer this question",
    )
    @app_commands.choices(question_type=QUESTION_TYPE_CHOICES)
    @require_guild
    async def question_add(
        self,
        interaction: discord.Interaction,
        survey_id: int,
        question_type: app_commands.Choice[str],
        prompt: app_commands.Range[str, 1, MAX_SURVEY_PROMPT_LENGTH],
        required: bool = True,
    ) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            question = await asyncio.to_thread(
                self.survey_service.add_question,
                interaction.guild.id,
                survey_id,
                str(prompt),
                question_type.value,
                required,
            )
            await safe_followup(
                interaction,
                content=(
                    f"✅ Added question **{question.position}** "
                    f"(ID `{question.question_id}`)."
                ),
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            await self.respond_error(interaction, str(exc))

    @question.command(name="edit", description="Edit a draft survey question")
    @app_commands.describe(
        survey_id="Survey number",
        question_id="Question ID shown by /survey preview",
        prompt="Replacement prompt",
        question_type="Replacement answer format",
        required="Replacement required/optional setting",
    )
    @app_commands.choices(question_type=QUESTION_TYPE_CHOICES)
    @require_guild
    async def question_edit(
        self,
        interaction: discord.Interaction,
        survey_id: int,
        question_id: int,
        prompt: app_commands.Range[str, 1, MAX_SURVEY_PROMPT_LENGTH] | None = None,
        question_type: app_commands.Choice[str] | None = None,
        required: bool | None = None,
    ) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            question = await asyncio.to_thread(
                self.survey_service.edit_question,
                interaction.guild.id,
                survey_id,
                question_id,
                prompt=str(prompt) if prompt is not None else None,
                question_type=question_type.value if question_type else None,
                required=required,
            )
            await safe_followup(
                interaction,
                content=f"✅ Updated question **{question.position}**.",
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            await self.respond_error(interaction, str(exc))

    @question.command(name="remove", description="Remove a draft survey question")
    @require_guild
    async def question_remove(
        self,
        interaction: discord.Interaction,
        survey_id: int,
        question_id: int,
    ) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            removed = await asyncio.to_thread(
                self.survey_service.remove_question,
                interaction.guild.id,
                survey_id,
                question_id,
            )
            if not removed:
                raise ValueError("Survey question not found")
            await safe_followup(
                interaction,
                content="✅ Removed and resequenced the draft question.",
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            await self.respond_error(interaction, str(exc))

    @question.command(name="move", description="Move a draft question to a new position")
    @require_guild
    async def question_move(
        self,
        interaction: discord.Interaction,
        survey_id: int,
        question_id: int,
        new_position: app_commands.Range[int, 1, 10],
    ) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            questions = await asyncio.to_thread(
                self.survey_service.move_question,
                interaction.guild.id,
                survey_id,
                question_id,
                int(new_position),
            )
            moved = next(
                question for question in questions if question.question_id == question_id
            )
            await safe_followup(
                interaction,
                content=(
                    f"✅ Moved question `{question_id}` to position "
                    f"**{moved.position}**."
                ),
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except (ValueError, StopIteration) as exc:
            message = str(exc) if str(exc) else "Survey question not found"
            await self.respond_error(interaction, message)

    @survey.command(name="send", description="Send a survey immediately by DM")
    @app_commands.describe(
        survey_id="Survey number",
        target="Recipient source",
        member="Required only for member targeting",
        role="Required only for role targeting",
        registered_only="For member/role targets, include only registered Cama players",
    )
    @app_commands.choices(target=TARGET_TYPE_CHOICES)
    @require_guild
    async def send(
        self,
        interaction: discord.Interaction,
        survey_id: int,
        target: app_commands.Choice[str],
        member: discord.Member | None = None,
        role: discord.Role | None = None,
        registered_only: bool = False,
    ) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            target_type = SurveyTargetType(target.value)
            recipient_ids = await self._resolve_recipients(
                interaction.guild,
                target_type,
                member=member,
                role=role,
                registered_only=registered_only,
            )
            await asyncio.to_thread(
                self.survey_service.open_survey,
                interaction.guild.id,
                survey_id,
                recipient_ids,
                target_type=target_type,
                registered_only=registered_only,
            )
            # The recipients are durably pending now; deliver in the
            # background instead of running a whole DM campaign inline under
            # the delivery lock and the 15-minute interaction token.
            self._start_send_dispatch(interaction.guild.id, survey_id)
            await safe_followup(
                interaction,
                content=(
                    f"✅ Survey **#{survey_id}** opened for "
                    f"**{len(recipient_ids)}** recipients. Delivering by DM — "
                    f"check `/survey results` for delivery progress."
                ),
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
            await self.respond_error(interaction, str(exc))
        except Exception as exc:
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
            logger.error("Survey send command failed (%s)", type(exc).__name__)
            await self.respond_error(interaction, "Couldn't send that survey right now.")

    @survey.command(name="retry", description="Retry failed DMs for an open survey")
    @require_guild
    async def retry(self, interaction: discord.Interaction, survey_id: int) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            async with self._delivery_lock:
                queued = await asyncio.to_thread(
                    self.survey_service.retry_failed_deliveries,
                    interaction.guild.id,
                    survey_id,
                )
                await self._dispatch_pending_locked()
            await self.reconcile_response_messages()
            await self._schedule_if_unconfirmed_deliveries()
            results = await asyncio.to_thread(
                self.survey_service.get_results,
                interaction.guild.id,
                survey_id,
            )
            await safe_followup(
                interaction,
                content=(
                    f"Retry requested for **{queued}** failed deliveries. "
                    f"Delivered now: **{results.delivered_count}/{results.recipient_count}**."
                ),
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
            await self.respond_error(interaction, str(exc))
        except Exception as exc:
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
            logger.error("Survey retry command failed (%s)", type(exc).__name__)
            await self.respond_error(interaction, "Couldn't retry that survey right now.")

    @survey.command(name="results", description="View anonymous survey results")
    @require_guild
    async def results(self, interaction: discord.Interaction, survey_id: int) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            results = await asyncio.to_thread(
                self.survey_service.get_results,
                interaction.guild.id,
                survey_id,
            )
            pages = _results_pages(results)
            view = (
                SurveyResultsView(interaction.user.id, pages, interaction)
                if len(pages) > 1
                else None
            )
            await safe_followup(
                interaction,
                embed=pages[0],
                view=view,
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            await self.respond_error(interaction, str(exc))
        except Exception as exc:
            logger.error("Survey results command failed (%s)", type(exc).__name__)
            await self.respond_error(interaction, "Couldn't load those results right now.")

    @survey.command(name="close", description="Close an open survey")
    @require_guild
    async def close(self, interaction: discord.Interaction, survey_id: int) -> None:
        if not await self._require_admin(interaction):
            return
        if not await safe_defer(interaction, ephemeral=True, thinking=True):
            return
        try:
            async with self._delivery_lock:
                await self.reconcile_unconfirmed_delivery_receipts(
                    interaction.guild.id,
                    survey_id,
                )
                survey = await asyncio.to_thread(
                    self.survey_service.close_survey,
                    interaction.guild.id,
                    survey_id,
                )
                # A transient history error before close remains durably
                # unconfirmed; make one immediate closed-campaign pass too.
                await self.reconcile_unconfirmed_delivery_receipts(
                    interaction.guild.id,
                    survey_id,
                )
                recoverable = await asyncio.to_thread(
                    self.survey_service.list_recoverable_response_sessions
                )
            sessions = [
                session
                for session in recoverable
                if session.survey.guild_id == interaction.guild.id
                and session.survey.survey_id == survey_id
            ]
            if sessions:
                await asyncio.gather(
                    *(self._reconcile_response_message(session) for session in sessions)
                )
            await self._schedule_if_unconfirmed_deliveries()
            await safe_followup(
                interaction,
                content=(
                    f"✅ Closed survey **#{survey.survey_id} — "
                    f"{escape_discord_text(survey.title)}**."
                ),
                ephemeral=True,
                allowed_mentions=_ALLOWED_MENTIONS,
            )
        except ValueError as exc:
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
            await self.respond_error(interaction, str(exc))
        except Exception as exc:
            self._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
            logger.error("Survey close command failed (%s)", type(exc).__name__)
            await self.respond_error(interaction, "Couldn't close that survey right now.")


async def setup(bot: commands.Bot) -> None:
    survey_service = getattr(bot, "survey_service", None)
    if survey_service is None:
        raise RuntimeError("survey_service must be initialized before commands.survey")
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("player_service must be initialized before commands.survey")
    cog = SurveyCommands(bot, survey_service, player_service)
    await bot.add_cog(cog)
    restored = await cog.restore_response_views()
    # cog_unload cancels in-flight dispatch tasks, so a mid-campaign extension
    # reload must resume pending deliveries itself instead of stranding
    # recipients until the next gateway reconnect.
    cog._schedule_delivery_recovery(_DELIVERY_RECEIPT_GRACE_SECONDS)
    logger.info("Survey command cog loaded; restored %d response views", restored)
