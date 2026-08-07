"""Focused command-layer tests for survey targeting and anonymous rendering."""

from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

from commands.survey import SurveyCommands, _results_pages
from domain.models.survey import (
    SurveyQuestionResult,
    SurveyQuestionType,
    SurveyResults,
    SurveyStatus,
    SurveyTargetType,
)
from repositories.survey_repository import SurveyRepository
from services.survey_service import SurveyService
from utils.embed_safety import validate_embed
from utils.formatting import escape_discord_text


def _member(member_id: int, guild, *, bot: bool = False):
    return SimpleNamespace(id=member_id, guild=guild, bot=bot)


@pytest.mark.asyncio
async def test_recipient_resolution_filters_fakes_bots_departed_and_registration():
    guild = SimpleNamespace(id=55, members=[])
    registered = _member(1, guild)
    unregistered = _member(2, guild)
    bot_member = _member(4, guild, bot=True)
    guild.members = [registered, unregistered, bot_member]
    player_service = MagicMock()
    player_service.get_all.return_value = [
        SimpleNamespace(discord_id=1),
        SimpleNamespace(discord_id=-1),  # fake player
        SimpleNamespace(discord_id=3),  # departed member
        SimpleNamespace(discord_id=4),  # bot
    ]
    cog = SurveyCommands(MagicMock(), MagicMock(), player_service)

    recipients = await cog._resolve_recipients(
        guild,
        SurveyTargetType.ALL_REGISTERED,
        member=None,
        role=None,
        registered_only=False,
    )
    assert recipients == [1]

    role = SimpleNamespace(id=88, guild=guild, members=guild.members)
    recipients = await cog._resolve_recipients(
        guild,
        SurveyTargetType.ROLE,
        member=None,
        role=role,
        registered_only=False,
    )
    assert recipients == [1, 2]

    registered_recipients = await cog._resolve_recipients(
        guild,
        SurveyTargetType.ROLE,
        member=None,
        role=role,
        registered_only=True,
    )
    assert registered_recipients == [1]

    with pytest.raises(ValueError, match="not a registered"):
        await cog._resolve_recipients(
            guild,
            SurveyTargetType.MEMBER,
            member=unregistered,
            role=None,
            registered_only=True,
        )


def test_result_pages_are_anonymous_safe_and_have_no_small_cohort_warning():
    results = SurveyResults(
        survey_id=9,
        title="Feedback @everyone",
        status=SurveyStatus.OPEN,
        recipient_count=1,
        delivered_count=1,
        started_count=1,
        completed_count=1,
        delivery_rate=1.0,
        started_rate=1.0,
        completion_rate=1.0,
        response_rate=1.0,
        questions=(
            SurveyQuestionResult(
                question_id=10,
                position=1,
                prompt="Why?",
                question_type=SurveyQuestionType.TEXT,
                response_count=1,
                skipped_count=0,
                nps_score=None,
                promoters=0,
                passives=0,
                detractors=0,
                rating_average=None,
                rating_distribution=(0, 0, 0, 0, 0),
                text_responses=("Because @admins are helpful",),
            ),
        ),
    )

    pages = _results_pages(results)
    rendered = "\n".join(str(page.to_dict()) for page in pages).lower()

    assert "discord_id" not in rendered
    assert "recipient_id" not in rendered
    assert "deanonym" not in rendered
    assert "fewer than five" not in rendered
    assert "small cohort" not in rendered
    assert "@admins" not in rendered
    assert r"\u200b" in rendered
    assert all(validate_embed(page) == [] for page in pages)


def test_zero_submission_report_has_real_questions_and_zero_aggregates(repo_db_path):
    service = SurveyService(SurveyRepository(repo_db_path))
    survey = service.create_survey(55, "No responses yet")
    service.add_question(55, survey.survey_id, "Recommend?", "nps")
    service.add_question(55, survey.survey_id, "Rate it", "rating")
    service.add_question(55, survey.survey_id, "Why?", "text", required=False)
    service.open_survey(
        55,
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

    results = service.get_results(55, survey.survey_id)
    pages = _results_pages(results)
    rendered = "\n".join(str(page.to_dict()) for page in pages)

    assert results.recipient_count == 1
    assert results.delivered_count == 1
    assert results.started_count == 0
    assert results.completed_count == 0
    assert results.delivery_rate == 1.0
    assert results.started_rate == 0.0
    assert results.completion_rate == 0.0
    assert results.response_rate == 0.0
    assert results.questions[0].nps_score is None
    assert results.questions[1].rating_average is None
    assert results.questions[1].rating_distribution == (0, 0, 0, 0, 0)
    assert results.questions[2].text_responses == ()
    assert rendered.count("N/A") >= 2
    assert "No written responses" in rendered
    assert all(validate_embed(page) == [] for page in pages)


def test_result_pages_preserve_complete_max_length_escaped_text_answer():
    answer = "@*" * 500
    results = SurveyResults(
        survey_id=9,
        title="Feedback",
        status=SurveyStatus.CLOSED,
        recipient_count=1,
        delivered_count=1,
        started_count=1,
        completed_count=1,
        delivery_rate=1.0,
        started_rate=1.0,
        completion_rate=1.0,
        response_rate=1.0,
        questions=(
            SurveyQuestionResult(
                question_id=10,
                position=1,
                prompt="*" * 1_000,
                question_type=SurveyQuestionType.TEXT,
                response_count=1,
                skipped_count=0,
                nps_score=None,
                promoters=0,
                passives=0,
                detractors=0,
                rating_average=None,
                rating_distribution=(0, 0, 0, 0, 0),
                text_responses=(answer,),
            ),
        ),
    )

    pages = _results_pages(results)
    rendered_answer = "".join(field.value for page in pages[1:] for field in page.fields)

    assert rendered_answer == escape_discord_text(answer)
    assert all(validate_embed(page) == [] for page in pages)
