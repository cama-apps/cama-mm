"""Focused input-policy tests for SurveyService."""

from __future__ import annotations

import pytest

from domain.models.survey import SurveyQuestionType
from repositories.survey_repository import SurveyRepository
from services.survey_service import SurveyService


@pytest.fixture
def survey_service(repo_db_path) -> SurveyService:
    return SurveyService(SurveyRepository(repo_db_path))


def test_create_and_question_input_limits(survey_service):
    with pytest.raises(ValueError, match="empty"):
        survey_service.create_survey(10, "  ")
    with pytest.raises(ValueError, match="100 characters"):
        survey_service.create_survey(10, "x" * 101)

    survey = survey_service.create_survey(10, "Trimmed title")
    with pytest.raises(ValueError, match="empty"):
        survey_service.add_question(10, survey.survey_id, " ", "nps")
    with pytest.raises(ValueError, match="Expected one of"):
        survey_service.add_question(10, survey.survey_id, "Question", "boolean")
    question = survey_service.add_question(
        10,
        survey.survey_id,
        " Recommend? ",
        SurveyQuestionType.NPS,
    )
    assert question.prompt == "Recommend?"


def test_target_shape_and_recipient_validation(survey_service):
    survey = survey_service.create_survey(10, "Targets")
    survey_service.add_question(10, survey.survey_id, "Recommend?", "nps")
    with pytest.raises(ValueError, match="already limited"):
        survey_service.open_survey(
            10,
            survey.survey_id,
            [101],
            target_type="all_registered",
            registered_only=True,
        )
    with pytest.raises(ValueError, match="exactly one recipient"):
        survey_service.open_survey(
            10,
            survey.survey_id,
            [101, 102],
            target_type="member",
        )
    with pytest.raises(ValueError, match="real Discord users|positive Discord ID"):
        survey_service.open_survey(
            10,
            survey.survey_id,
            [-1],
            target_type="all_registered",
        )


@pytest.mark.parametrize(
    ("question_type", "invalid_values", "valid_value"),
    [
        ("nps", (-1, 11, 5.5, True, "10"), 10),
        ("rating", (0, 6, 3.5, True, "5"), 5),
        ("text", ("", "   ", "x" * 1_001, 5), "Useful feedback"),
    ],
)
def test_answer_validation_by_question_type(
    repo_db_path,
    question_type,
    invalid_values,
    valid_value,
):
    service = SurveyService(SurveyRepository(repo_db_path))
    survey = service.create_survey(10, f"Validation {question_type}")
    question = service.add_question(10, survey.survey_id, "Question", question_type)
    service.open_survey(
        10,
        survey.survey_id,
        [101],
        target_type="member",
    )
    claim = service.claim_deliveries()[0]
    service.mark_delivery_sent(
        claim.recipient_id,
        claim.attempt_count,
        501,
        601,
    )

    for invalid_value in invalid_values:
        with pytest.raises(ValueError):
            service.answer_question(
                survey.survey_id,
                101,
                question.question_id,
                invalid_value,
            )
    session = service.answer_question(
        survey.survey_id,
        101,
        question.question_id,
        valid_value,
    )
    assert session.recipient.in_review is True


def test_required_question_cannot_be_skipped_but_optional_can(survey_service):
    survey = survey_service.create_survey(10, "Skipping")
    required = survey_service.add_question(10, survey.survey_id, "Required", "nps")
    optional = survey_service.add_question(
        10,
        survey.survey_id,
        "Optional",
        "text",
        required=False,
    )
    survey_service.open_survey(
        10,
        survey.survey_id,
        [101],
        target_type="member",
    )
    claim = survey_service.claim_deliveries()[0]
    survey_service.mark_delivery_sent(
        claim.recipient_id,
        claim.attempt_count,
        501,
        601,
    )

    with pytest.raises(ValueError, match="required"):
        survey_service.skip_question(survey.survey_id, 101, required.question_id)
    survey_service.answer_question(survey.survey_id, 101, required.question_id, 8)
    session = survey_service.skip_question(survey.survey_id, 101, optional.question_id)
    assert session.recipient.in_review is True
    assert session.answers[-1].skipped is True
    duplicate = survey_service.skip_question(
        survey.survey_id,
        101,
        optional.question_id,
    )
    assert duplicate.recipient.in_review is True
    assert len(duplicate.answers) == 2
