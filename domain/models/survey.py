"""Domain records for one-off, guild-scoped Discord surveys.

Respondent identifiers intentionally appear only on delivery/session records.
The aggregate result records have no respondent key and cannot represent
cross-question correlations.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import StrEnum

MAX_SURVEY_QUESTIONS = 10
MAX_SURVEY_TITLE_LENGTH = 100
MAX_SURVEY_PROMPT_LENGTH = 1_000
MAX_TEXT_ANSWER_LENGTH = 1_000


class SurveyStatus(StrEnum):
    DRAFT = "draft"
    OPEN = "open"
    CLOSED = "closed"


class SurveyQuestionType(StrEnum):
    NPS = "nps"
    RATING = "rating"
    TEXT = "text"


class SurveyTargetType(StrEnum):
    ALL_REGISTERED = "all_registered"
    MEMBER = "member"
    ROLE = "role"


class SurveyDeliveryStatus(StrEnum):
    PENDING = "pending"
    SENDING = "sending"
    SENT = "sent"
    FAILED = "failed"


class SurveyError(ValueError):
    """Base class for expected survey validation/lifecycle failures."""


class SurveyNotFoundError(SurveyError):
    """Raised when a survey is absent from the requested guild."""


class SurveyStateError(SurveyError):
    """Raised when an operation is invalid for the survey lifecycle state."""


class SurveyResponseError(SurveyError):
    """Raised when a respondent/session operation is invalid."""


@dataclass(frozen=True, slots=True)
class Survey:
    survey_id: int
    guild_id: int
    title: str
    status: SurveyStatus
    target_type: SurveyTargetType | None
    registered_only: bool
    created_at: int
    opened_at: int | None
    closed_at: int | None


@dataclass(frozen=True, slots=True)
class SurveyQuestion:
    question_id: int
    survey_id: int
    position: int
    prompt: str
    question_type: SurveyQuestionType
    required: bool
    created_at: int


@dataclass(frozen=True, slots=True)
class SurveyRecipient:
    """Internal delivery/session state; never include this in admin results."""

    recipient_id: int
    survey_id: int
    guild_id: int
    discord_id: int
    delivery_status: SurveyDeliveryStatus
    attempt_count: int
    claimed_at: int | None
    dm_channel_id: int | None
    dm_message_id: int | None
    ui_channel_id: int | None
    ui_message_id: int | None
    current_question_id: int | None
    in_review: bool
    started_at: int | None
    submitted_at: int | None
    last_error: str | None
    created_at: int
    updated_at: int


@dataclass(frozen=True, slots=True)
class SurveyDraftAnswer:
    """One respondent-local draft answer without a respondent identifier."""

    question_id: int
    numeric_value: int | None
    text_value: str | None
    skipped: bool
    updated_at: int


@dataclass(frozen=True, slots=True)
class SurveySession:
    """Private respondent session used by DM controls and restart recovery."""

    survey: Survey
    recipient: SurveyRecipient
    questions: tuple[SurveyQuestion, ...]
    answers: tuple[SurveyDraftAnswer, ...]

    @property
    def current_question(self) -> SurveyQuestion | None:
        question_id = self.recipient.current_question_id
        if question_id is None:
            return None
        return next(
            (question for question in self.questions if question.question_id == question_id),
            None,
        )


@dataclass(frozen=True, slots=True)
class SurveyQuestionResult:
    """Anonymous aggregate for exactly one question."""

    question_id: int
    position: int
    prompt: str
    question_type: SurveyQuestionType
    response_count: int
    skipped_count: int
    nps_score: int | None
    promoters: int
    passives: int
    detractors: int
    rating_average: float | None
    # Counts in score order 1, 2, 3, 4, 5.
    rating_distribution: tuple[int, int, int, int, int]
    text_responses: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class SurveyResults:
    """Admin-safe survey summary with no respondent identity/correlation."""

    survey_id: int
    title: str
    status: SurveyStatus
    recipient_count: int
    delivered_count: int
    started_count: int
    completed_count: int
    delivery_rate: float
    started_rate: float
    completion_rate: float
    response_rate: float
    questions: tuple[SurveyQuestionResult, ...]
