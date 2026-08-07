"""Validation and lifecycle policy for one-off Discord surveys."""

from __future__ import annotations

from enum import Enum

from domain.models.survey import (
    MAX_SURVEY_PROMPT_LENGTH,
    MAX_SURVEY_TITLE_LENGTH,
    MAX_TEXT_ANSWER_LENGTH,
    Survey,
    SurveyDeliveryStatus,
    SurveyNotFoundError,
    SurveyQuestion,
    SurveyQuestionType,
    SurveyRecipient,
    SurveyResults,
    SurveySession,
    SurveyStateError,
    SurveyStatus,
    SurveyTargetType,
)
from repositories.interfaces import ISurveyRepository


class SurveyService:
    """Validate survey input and delegate atomic state changes to the repository."""

    def __init__(self, survey_repo: ISurveyRepository):
        self.repo = survey_repo

    @staticmethod
    def _enum_value(enum_type, value):
        raw = value.value if isinstance(value, Enum) else str(value).strip().lower()
        try:
            return enum_type(raw)
        except ValueError as exc:
            choices = ", ".join(item.value for item in enum_type)
            raise ValueError(f"Expected one of: {choices}") from exc

    @staticmethod
    def _clean_text(value: str, *, label: str, max_length: int) -> str:
        if not isinstance(value, str):
            raise ValueError(f"{label} must be text")
        cleaned = value.strip()
        if not cleaned:
            raise ValueError(f"{label} cannot be empty")
        if len(cleaned) > max_length:
            raise ValueError(f"{label} cannot exceed {max_length} characters")
        return cleaned

    @staticmethod
    def _positive_id(value: int, *, label: str) -> int:
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            raise ValueError(f"{label} must be a positive Discord ID")
        return value

    def create_survey(self, guild_id: int, title: str) -> Survey:
        clean_title = self._clean_text(
            title,
            label="Survey title",
            max_length=MAX_SURVEY_TITLE_LENGTH,
        )
        return self.repo.create_survey(guild_id, clean_title)

    def list_surveys(
        self,
        guild_id: int,
        status: SurveyStatus | str | None = None,
    ) -> list[Survey]:
        normalized_status = (
            self._enum_value(SurveyStatus, status).value if status is not None else None
        )
        return self.repo.list_surveys(guild_id, normalized_status)

    def get_survey(self, guild_id: int, survey_id: int) -> Survey | None:
        return self.repo.get_survey(guild_id, survey_id)

    def delete_survey(self, guild_id: int, survey_id: int) -> bool:
        return self.repo.delete_survey(guild_id, survey_id)

    def get_questions(self, guild_id: int, survey_id: int) -> list[SurveyQuestion]:
        return self.repo.get_questions(guild_id, survey_id)

    def add_question(
        self,
        guild_id: int,
        survey_id: int,
        prompt: str,
        question_type: SurveyQuestionType | str,
        required: bool = True,
    ) -> SurveyQuestion:
        clean_prompt = self._clean_text(
            prompt,
            label="Question prompt",
            max_length=MAX_SURVEY_PROMPT_LENGTH,
        )
        normalized_type = self._enum_value(SurveyQuestionType, question_type)
        if not isinstance(required, bool):
            raise ValueError("required must be true or false")
        return self.repo.add_question(
            guild_id,
            survey_id,
            clean_prompt,
            normalized_type.value,
            required,
        )

    def edit_question(
        self,
        guild_id: int,
        survey_id: int,
        question_id: int,
        *,
        prompt: str | None = None,
        question_type: SurveyQuestionType | str | None = None,
        required: bool | None = None,
    ) -> SurveyQuestion:
        if prompt is None and question_type is None and required is None:
            raise ValueError("Provide at least one question change")
        clean_prompt = (
            self._clean_text(
                prompt,
                label="Question prompt",
                max_length=MAX_SURVEY_PROMPT_LENGTH,
            )
            if prompt is not None
            else None
        )
        normalized_type = (
            self._enum_value(SurveyQuestionType, question_type).value
            if question_type is not None
            else None
        )
        if required is not None and not isinstance(required, bool):
            raise ValueError("required must be true or false")
        return self.repo.edit_question(
            guild_id,
            survey_id,
            question_id,
            prompt=clean_prompt,
            question_type=normalized_type,
            required=required,
        )

    def remove_question(self, guild_id: int, survey_id: int, question_id: int) -> bool:
        return self.repo.remove_question(guild_id, survey_id, question_id)

    def move_question(
        self,
        guild_id: int,
        survey_id: int,
        question_id: int,
        new_position: int,
    ) -> list[SurveyQuestion]:
        if isinstance(new_position, bool) or not isinstance(new_position, int):
            raise ValueError("Question position must be an integer")
        return self.repo.move_question(guild_id, survey_id, question_id, new_position)

    def open_survey(
        self,
        guild_id: int,
        survey_id: int,
        recipient_ids: list[int],
        *,
        target_type: SurveyTargetType | str,
        registered_only: bool = False,
    ) -> Survey:
        normalized_target = self._enum_value(SurveyTargetType, target_type)
        if not isinstance(registered_only, bool):
            raise ValueError("registered_only must be true or false")
        if normalized_target is SurveyTargetType.ALL_REGISTERED and registered_only:
            raise ValueError("all_registered targeting is already limited to registered users")

        if not isinstance(recipient_ids, list):
            recipient_ids = list(recipient_ids)
        clean_recipient_ids = list(
            dict.fromkeys(
                self._positive_id(recipient_id, label="Survey recipient")
                for recipient_id in recipient_ids
            )
        )
        if not clean_recipient_ids:
            raise ValueError("The survey target has no eligible recipients")
        if normalized_target is SurveyTargetType.MEMBER and len(clean_recipient_ids) != 1:
            raise ValueError("member targeting requires exactly one recipient")
        return self.repo.open_survey(
            guild_id,
            survey_id,
            clean_recipient_ids,
            target_type=normalized_target.value,
            registered_only=registered_only,
        )

    def close_survey(self, guild_id: int, survey_id: int) -> Survey:
        return self.repo.close_survey(guild_id, survey_id)

    def claim_deliveries(
        self,
        limit: int = 10,
        stale_after_seconds: int = 300,
    ) -> list[SurveyRecipient]:
        return self.repo.claim_deliveries(limit, stale_after_seconds)

    def renew_delivery_claim(
        self,
        recipient_id: int,
        expected_attempt_count: int,
    ) -> SurveyRecipient:
        self._positive_id(recipient_id, label="Survey recipient")
        self._positive_id(expected_attempt_count, label="Delivery attempt")
        return self.repo.renew_delivery_claim(
            recipient_id,
            expected_attempt_count,
        )

    def recover_stale_deliveries(self, stale_after_seconds: int = 300) -> int:
        return self.repo.recover_stale_deliveries(stale_after_seconds)

    def list_unconfirmed_deliveries(
        self,
        guild_id: int | None = None,
        survey_id: int | None = None,
        sending_stale_after_seconds: int = 300,
        failed_grace_seconds: int = 30,
    ) -> list[SurveyRecipient]:
        return self.repo.list_unconfirmed_deliveries(
            guild_id,
            survey_id,
            sending_stale_after_seconds,
            failed_grace_seconds,
        )

    def mark_delivery_sent(
        self,
        recipient_id: int,
        expected_attempt_count: int,
        dm_channel_id: int,
        dm_message_id: int,
    ) -> SurveyRecipient:
        self._positive_id(recipient_id, label="Survey recipient")
        self._positive_id(expected_attempt_count, label="Delivery attempt")
        self._positive_id(dm_channel_id, label="DM channel")
        self._positive_id(dm_message_id, label="DM message")
        return self.repo.mark_delivery_sent(
            recipient_id,
            expected_attempt_count,
            dm_channel_id,
            dm_message_id,
        )

    def mark_delivery_failed(
        self,
        recipient_id: int,
        expected_attempt_count: int,
        error: str,
    ) -> SurveyRecipient:
        self._positive_id(recipient_id, label="Survey recipient")
        self._positive_id(expected_attempt_count, label="Delivery attempt")
        return self.repo.mark_delivery_failed(
            recipient_id,
            expected_attempt_count,
            error,
        )

    def reconcile_delivery_sent(
        self,
        recipient_id: int,
        expected_attempt_count: int,
        dm_channel_id: int,
        dm_message_id: int,
    ) -> SurveyRecipient:
        self._positive_id(recipient_id, label="Survey recipient")
        self._positive_id(expected_attempt_count, label="Delivery attempt")
        self._positive_id(dm_channel_id, label="DM channel")
        self._positive_id(dm_message_id, label="DM message")
        return self.repo.reconcile_delivery_sent(
            recipient_id,
            expected_attempt_count,
            dm_channel_id,
            dm_message_id,
        )

    def mark_delivery_receipt_checked(
        self,
        recipient_id: int,
        expected_attempt_count: int,
        expected_claimed_at: int | None,
        expected_delivery_status: SurveyDeliveryStatus | str,
        expected_updated_at: int,
    ) -> bool:
        self._positive_id(recipient_id, label="Survey recipient")
        self._positive_id(expected_attempt_count, label="Delivery attempt")
        normalized_status = self._enum_value(
            SurveyDeliveryStatus,
            expected_delivery_status,
        )
        return self.repo.mark_delivery_receipt_checked(
            recipient_id,
            expected_attempt_count,
            expected_claimed_at,
            normalized_status.value,
            expected_updated_at,
        )

    def retry_failed_deliveries(self, guild_id: int, survey_id: int) -> int:
        return self.repo.retry_failed_deliveries(guild_id, survey_id)

    def queue_requested_delivery_retries(self) -> int:
        return self.repo.queue_requested_delivery_retries()

    def get_response_session(self, survey_id: int, discord_id: int) -> SurveySession | None:
        self._positive_id(discord_id, label="Respondent")
        return self.repo.get_response_session(survey_id, discord_id)

    def list_recoverable_response_sessions(self) -> list[SurveySession]:
        return self.repo.list_recoverable_response_sessions()

    @staticmethod
    def _question_for_session(
        session: SurveySession,
        question_id: int,
    ) -> SurveyQuestion:
        question = next(
            (
                candidate
                for candidate in session.questions
                if candidate.question_id == question_id
            ),
            None,
        )
        if question is None:
            raise SurveyNotFoundError("Question does not belong to this survey")
        return question

    def answer_question(
        self,
        survey_id: int,
        discord_id: int,
        question_id: int,
        value: int | str,
    ) -> SurveySession:
        session = self.get_response_session(survey_id, discord_id)
        if session is None:
            raise SurveyNotFoundError("This survey was not sent to you")
        if session.survey.status is not SurveyStatus.OPEN:
            raise SurveyStateError("This survey is closed")
        if session.recipient.submitted_at is not None:
            raise SurveyStateError("This response has already been submitted")
        if session.recipient.delivery_status is not SurveyDeliveryStatus.SENT:
            raise SurveyStateError("This survey has not been delivered")
        question = self._question_for_session(session, question_id)

        numeric_value: int | None = None
        text_value: str | None = None
        if question.question_type is SurveyQuestionType.NPS:
            if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= 10:
                raise ValueError("NPS answers must be a whole number from 0 to 10")
            numeric_value = value
        elif question.question_type is SurveyQuestionType.RATING:
            if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= 5:
                raise ValueError("Rating answers must be a whole number from 1 to 5")
            numeric_value = value
        else:
            text_value = self._clean_text(
                value,
                label="Text answer",
                max_length=MAX_TEXT_ANSWER_LENGTH,
            )
        return self.repo.save_answer(
            survey_id,
            discord_id,
            question_id,
            numeric_value=numeric_value,
            text_value=text_value,
        )

    def skip_question(
        self,
        survey_id: int,
        discord_id: int,
        question_id: int,
    ) -> SurveySession:
        session = self.get_response_session(survey_id, discord_id)
        if session is None:
            raise SurveyNotFoundError("This survey was not sent to you")
        if session.survey.status is not SurveyStatus.OPEN:
            raise SurveyStateError("This survey is closed")
        if session.recipient.submitted_at is not None:
            raise SurveyStateError("This response has already been submitted")
        if session.recipient.delivery_status is not SurveyDeliveryStatus.SENT:
            raise SurveyStateError("This survey has not been delivered")
        question = self._question_for_session(session, question_id)
        if question.required:
            raise ValueError("This question is required")
        return self.repo.skip_question(survey_id, discord_id, question_id)

    def begin_review(self, survey_id: int, discord_id: int) -> SurveySession:
        return self.repo.begin_review(survey_id, discord_id)

    def select_response_question(
        self,
        survey_id: int,
        discord_id: int,
        question_id: int,
    ) -> SurveySession:
        return self.repo.select_response_question(survey_id, discord_id, question_id)

    def submit_response(self, survey_id: int, discord_id: int) -> bool:
        return self.repo.submit_response(survey_id, discord_id)

    def finalize_response_ui(
        self,
        survey_id: int,
        discord_id: int,
        expected_message_id: int,
    ) -> bool:
        self._positive_id(discord_id, label="Respondent")
        self._positive_id(expected_message_id, label="DM message")
        return self.repo.finalize_response_ui(
            survey_id,
            discord_id,
            expected_message_id,
        )

    def get_results(self, guild_id: int, survey_id: int) -> SurveyResults:
        survey = self.repo.get_survey(guild_id, survey_id)
        if survey is None:
            raise SurveyNotFoundError("Survey not found in this server")
        if survey.status is SurveyStatus.DRAFT:
            raise SurveyStateError("Results are available after the survey is sent")
        return self.repo.get_results(guild_id, survey_id)
