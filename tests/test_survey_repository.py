"""Focused persistence tests for one-off surveys."""

from __future__ import annotations

import sqlite3
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict
from threading import Event

import pytest

from domain.models.survey import (
    MAX_SURVEY_QUESTIONS,
    SurveyDeliveryStatus,
    SurveyResponseError,
    SurveyStateError,
    SurveyStatus,
)
from infrastructure.schema_manager import SchemaManager
from repositories.survey_repository import SurveyRepository
from services.survey_service import SurveyService


def _service(repo_db_path: str) -> SurveyService:
    return SurveyService(SurveyRepository(repo_db_path))


class _PausingQuestionReadSurveyRepository(SurveyRepository):
    """Pause a multi-query read after its first rows have pinned a snapshot."""

    def __init__(self, db_path: str, reached: Event, release: Event):
        super().__init__(db_path)
        self.reached = reached
        self.release = release

    def _get_questions_with_cursor(self, cursor, guild_id: int, survey_id: int):
        self.reached.set()
        if not self.release.wait(timeout=5):
            raise TimeoutError("test did not release the paused survey read")
        return super()._get_questions_with_cursor(cursor, guild_id, survey_id)


def _open_and_deliver(
    service: SurveyService,
    *,
    guild_id: int,
    survey_id: int,
    recipient_ids: list[int],
) -> None:
    service.open_survey(
        guild_id,
        survey_id,
        recipient_ids,
        target_type="all_registered",
    )
    claims = service.claim_deliveries(limit=len(recipient_ids))
    assert {claim.discord_id for claim in claims} == set(recipient_ids)
    for claim in claims:
        service.mark_delivery_sent(
            claim.recipient_id,
            claim.attempt_count,
            dm_channel_id=10_000 + claim.discord_id,
            dm_message_id=20_000 + claim.discord_id,
        )


def test_nps_rounding_uses_nearest_whole_with_conventional_ties():
    assert SurveyRepository._round_ratio_to_nearest(100, 8) == 13
    assert SurveyRepository._round_ratio_to_nearest(-100, 8) == -13
    assert SurveyRepository._round_ratio_to_nearest(100, 6) == 17


def test_delivery_migrations_quarantine_legacy_claims_and_add_retry_intent():
    with sqlite3.connect(":memory:") as conn:
        conn.row_factory = sqlite3.Row
        conn.execute(
            """
            CREATE TABLE survey_recipients (
                recipient_id INTEGER PRIMARY KEY,
                delivery_status TEXT NOT NULL,
                attempt_count INTEGER NOT NULL,
                claimed_at INTEGER,
                updated_at INTEGER NOT NULL
            )
            """
        )
        conn.execute(
            """
            INSERT INTO survey_recipients (
                recipient_id, delivery_status, attempt_count, claimed_at, updated_at
            ) VALUES (1, 'pending', 2, NULL, 123)
            """
        )

        SchemaManager(":memory:")._migration_add_survey_receipt_checked_attempt(
            conn.cursor()
        )
        SchemaManager(":memory:")._migration_add_survey_retry_requested(conn.cursor())
        row = conn.execute("SELECT * FROM survey_recipients").fetchone()

    assert row["delivery_status"] == "sending"
    assert row["claimed_at"] == 123
    assert row["receipt_checked_attempt"] == 0
    assert row["retry_requested"] == 0


def test_unchecked_legacy_pending_attempt_cannot_be_claimed(repo_db_path, monkeypatch):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Legacy pending")
    service.add_question(10, survey.survey_id, "Recommend?", "nps")
    service.open_survey(10, survey.survey_id, [101], target_type="member")
    monkeypatch.setattr(service.repo, "_now", lambda: 1_000)
    first = service.claim_deliveries()[0]
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            """
            UPDATE survey_recipients
            SET delivery_status = 'pending', receipt_checked_attempt = 0,
                claimed_at = NULL, updated_at = 1
            WHERE recipient_id = ?
            """,
            (first.recipient_id,),
        )

    assert service.claim_deliveries() == []
    unconfirmed = service.list_unconfirmed_deliveries(
        10,
        survey.survey_id,
        failed_grace_seconds=0,
    )
    assert [recipient.recipient_id for recipient in unconfirmed] == [
        first.recipient_id
    ]


def test_response_session_reads_recipient_and_answers_from_one_snapshot(repo_db_path):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Session snapshot")
    first = service.add_question(10, survey.survey_id, "First", "nps")
    service.add_question(10, survey.survey_id, "Second", "rating")
    _open_and_deliver(
        service,
        guild_id=10,
        survey_id=survey.survey_id,
        recipient_ids=[101],
    )
    reached = Event()
    release = Event()
    reader = _PausingQuestionReadSurveyRepository(repo_db_path, reached, release)

    with ThreadPoolExecutor(max_workers=1) as executor:
        future = executor.submit(reader.get_response_session, survey.survey_id, 101)
        assert reached.wait(timeout=5)
        try:
            service.answer_question(survey.survey_id, 101, first.question_id, 10)
        finally:
            release.set()
        snapshot = future.result(timeout=5)

    assert snapshot is not None
    assert snapshot.recipient.current_question_id == first.question_id
    assert snapshot.answers == ()


def test_open_results_metrics_and_answers_share_one_snapshot(repo_db_path):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Results snapshot")
    question = service.add_question(10, survey.survey_id, "Recommend?", "nps")
    _open_and_deliver(
        service,
        guild_id=10,
        survey_id=survey.survey_id,
        recipient_ids=[101],
    )
    service.answer_question(survey.survey_id, 101, question.question_id, 10)
    reached = Event()
    release = Event()
    reader = _PausingQuestionReadSurveyRepository(repo_db_path, reached, release)

    with ThreadPoolExecutor(max_workers=1) as executor:
        future = executor.submit(reader.get_results, 10, survey.survey_id)
        assert reached.wait(timeout=5)
        try:
            assert service.submit_response(survey.survey_id, 101) is True
        finally:
            release.set()
        snapshot = future.result(timeout=5)

    assert snapshot.completed_count == 0
    assert snapshot.questions[0].response_count == 0


def test_schema_is_guild_scoped_and_draft_questions_are_resequenced(repo_db_path):
    service = _service(repo_db_path)
    first = service.create_survey(10, "First guild")
    other = service.create_survey(20, "Other guild")

    one = service.add_question(10, first.survey_id, "One", "nps")
    service.add_question(10, first.survey_id, "Two", "rating")
    three = service.add_question(10, first.survey_id, "Three", "text", required=False)

    moved = service.move_question(10, first.survey_id, three.question_id, 1)
    assert [(question.position, question.prompt) for question in moved] == [
        (1, "Three"),
        (2, "One"),
        (3, "Two"),
    ]
    assert service.remove_question(10, first.survey_id, one.question_id) is True
    assert [(question.position, question.prompt) for question in service.get_questions(10, first.survey_id)] == [
        (1, "Three"),
        (2, "Two"),
    ]

    assert service.get_survey(20, first.survey_id) is None
    with pytest.raises(ValueError, match="not found"):
        service.get_questions(20, first.survey_id)
    assert [survey.survey_id for survey in service.list_surveys(20)] == [other.survey_id]

    with sqlite3.connect(repo_db_path) as conn:
        for table in (
            "surveys",
            "survey_questions",
            "survey_recipients",
            "survey_answers",
        ):
            columns = {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}
            assert "guild_id" in columns
        survey_columns = {
            row[1] for row in conn.execute("PRAGMA table_info(surveys)")
        }
        assert "created_by" not in survey_columns
        assert "target_id" not in survey_columns
        recipient_columns = {
            row[1] for row in conn.execute("PRAGMA table_info(survey_recipients)")
        }
        assert "controls_finalized" in recipient_columns
        assert "receipt_checked_attempt" in recipient_columns
        assert "retry_requested" in recipient_columns


def test_max_questions_and_open_campaign_are_immutable(repo_db_path):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Question limit")
    for index in range(MAX_SURVEY_QUESTIONS):
        service.add_question(10, survey.survey_id, f"Question {index}", "rating")

    with pytest.raises(SurveyStateError, match="at most"):
        service.add_question(10, survey.survey_id, "Too many", "rating")

    service.open_survey(
        10,
        survey.survey_id,
        [101],
        target_type="member",
    )
    with pytest.raises(SurveyStateError, match="immutable"):
        service.edit_question(
            10,
            survey.survey_id,
            service.get_questions(10, survey.survey_id)[0].question_id,
            prompt="Changed",
        )
    with pytest.raises(SurveyStateError, match="immutable"):
        service.remove_question(
            10,
            survey.survey_id,
            service.get_questions(10, survey.survey_id)[0].question_id,
        )
    with pytest.raises(SurveyStateError, match="Only draft"):
        service.delete_survey(10, survey.survey_id)
    with pytest.raises(SurveyStateError, match="immutable"):
        service.open_survey(
            10,
            survey.survey_id,
            [101],
            target_type="member",
        )


def test_delivery_claim_failure_retry_and_stale_recovery(repo_db_path, monkeypatch):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Delivery")
    service.add_question(10, survey.survey_id, "Recommend?", "nps")
    service.open_survey(
        10,
        survey.survey_id,
        [101, 102],
        target_type="role",
    )

    now = 1_000
    monkeypatch.setattr(service.repo, "_now", lambda: now)
    first = service.claim_deliveries(limit=1, stale_after_seconds=300)[0]
    assert first.delivery_status is SurveyDeliveryStatus.SENDING
    assert first.attempt_count == 1
    assert service.list_unconfirmed_deliveries(10, survey.survey_id) == []
    renewed = service.renew_delivery_claim(first.recipient_id, first.attempt_count)
    assert renewed.delivery_status is SurveyDeliveryStatus.SENDING
    failed = service.mark_delivery_failed(
        first.recipient_id,
        first.attempt_count,
        "DMs disabled",
    )
    assert failed.delivery_status is SurveyDeliveryStatus.FAILED
    assert service.mark_delivery_receipt_checked(
        renewed.recipient_id,
        renewed.attempt_count,
        renewed.claimed_at,
        renewed.delivery_status,
        renewed.updated_at,
    ) is False
    assert [
        recipient.recipient_id
        for recipient in service.list_unconfirmed_deliveries(
            10,
            survey.survey_id,
            failed_grace_seconds=0,
        )
    ] == [first.recipient_id]
    assert service.mark_delivery_receipt_checked(
        first.recipient_id,
        first.attempt_count,
        failed.claimed_at,
        failed.delivery_status,
        failed.updated_at,
    ) is True
    assert service.retry_failed_deliveries(10, survey.survey_id) == 1

    reclaimed = service.claim_deliveries(limit=1, stale_after_seconds=300)[0]
    assert reclaimed.recipient_id == first.recipient_id
    assert reclaimed.attempt_count == 2
    with pytest.raises(SurveyStateError, match="no longer active"):
        service.mark_delivery_failed(
            first.recipient_id,
            first.attempt_count,
            "late failure from stale worker",
        )
    with pytest.raises(SurveyStateError, match="no longer active"):
        service.mark_delivery_sent(
            first.recipient_id,
            first.attempt_count,
            999,
            999,
        )
    assert service.mark_delivery_receipt_checked(
        first.recipient_id,
        first.attempt_count,
        first.claimed_at,
        first.delivery_status,
        first.updated_at,
    ) is False
    service.mark_delivery_sent(
        reclaimed.recipient_id,
        reclaimed.attempt_count,
        501,
        601,
    )

    stranded = service.claim_deliveries(limit=1, stale_after_seconds=300)[0]
    assert stranded.discord_id == 102
    now = 1_301
    stale_snapshot = service.list_unconfirmed_deliveries(10, survey.survey_id)[0]
    now = 1_302
    renewed_stranded = service.renew_delivery_claim(
        stranded.recipient_id,
        stranded.attempt_count,
    )
    assert service.mark_delivery_receipt_checked(
        stale_snapshot.recipient_id,
        stale_snapshot.attempt_count,
        stale_snapshot.claimed_at,
        stale_snapshot.delivery_status,
        stale_snapshot.updated_at,
    ) is False
    now = 1_603
    eligible = service.list_unconfirmed_deliveries(10, survey.survey_id)[0]
    assert eligible.claimed_at == renewed_stranded.claimed_at
    assert service.recover_stale_deliveries(300) == 0
    assert service.mark_delivery_receipt_checked(
        eligible.recipient_id,
        eligible.attempt_count,
        eligible.claimed_at,
        eligible.delivery_status,
        eligible.updated_at,
    ) is True
    with pytest.raises(SurveyStateError, match="no longer active"):
        service.renew_delivery_claim(
            eligible.recipient_id,
            eligible.attempt_count,
        )
    recovered = service.claim_deliveries(limit=1, stale_after_seconds=300)[0]
    assert recovered.recipient_id == stranded.recipient_id
    assert recovered.attempt_count == 2
    service.mark_delivery_sent(
        recovered.recipient_id,
        recovered.attempt_count,
        502,
        602,
    )

    sessions = [
        service.get_response_session(survey.survey_id, discord_id)
        for discord_id in (101, 102)
    ]
    assert all(session is not None for session in sessions)
    assert all(session.recipient.ui_message_id is not None for session in sessions)


def test_retry_intent_waits_for_receipt_grace_then_queues_automatically(
    repo_db_path,
    monkeypatch,
):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Durable retry")
    service.add_question(10, survey.survey_id, "Recommend?", "nps")
    service.open_survey(10, survey.survey_id, [101], target_type="member")

    now = 1_000
    monkeypatch.setattr(service.repo, "_now", lambda: now)
    claim = service.claim_deliveries()[0]
    failed = service.mark_delivery_failed(
        claim.recipient_id,
        claim.attempt_count,
        "Forbidden",
    )

    assert service.retry_failed_deliveries(10, survey.survey_id) == 1
    assert service.claim_deliveries() == []
    service = _service(repo_db_path)
    monkeypatch.setattr(service.repo, "_now", lambda: now)
    assert service.claim_deliveries() == []
    now = 1_029
    assert service.list_unconfirmed_deliveries(10, survey.survey_id) == []

    now = 1_030
    eligible = service.list_unconfirmed_deliveries(10, survey.survey_id)
    assert [recipient.recipient_id for recipient in eligible] == [claim.recipient_id]
    assert service.mark_delivery_receipt_checked(
        eligible[0].recipient_id,
        eligible[0].attempt_count,
        eligible[0].claimed_at,
        eligible[0].delivery_status,
        eligible[0].updated_at,
    ) is True
    assert service.queue_requested_delivery_retries() == 1

    retried = service.claim_deliveries()[0]
    assert retried.recipient_id == failed.recipient_id
    assert retried.attempt_count == 2


def test_close_clears_retry_intent_and_closed_campaign_cannot_be_queued(
    repo_db_path,
    monkeypatch,
):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Close requested retry")
    service.add_question(10, survey.survey_id, "Recommend?", "nps")
    service.open_survey(10, survey.survey_id, [101], target_type="member")

    now = 1_000
    monkeypatch.setattr(service.repo, "_now", lambda: now)
    claim = service.claim_deliveries()[0]
    service.mark_delivery_failed(claim.recipient_id, claim.attempt_count, "Forbidden")
    assert service.retry_failed_deliveries(10, survey.survey_id) == 1

    service.close_survey(10, survey.survey_id)
    with sqlite3.connect(repo_db_path) as conn:
        retry_requested = conn.execute(
            "SELECT retry_requested FROM survey_recipients WHERE recipient_id = ?",
            (claim.recipient_id,),
        ).fetchone()[0]
    assert retry_requested == 0
    now = 1_030
    failed = service.list_unconfirmed_deliveries(10, survey.survey_id)[0]
    assert service.mark_delivery_receipt_checked(
        failed.recipient_id,
        failed.attempt_count,
        failed.claimed_at,
        failed.delivery_status,
        failed.updated_at,
    ) is True
    assert service.queue_requested_delivery_retries() == 0
    assert service.claim_deliveries() == []


def test_close_atomically_fails_a_receipt_checked_sending_attempt(
    repo_db_path,
    monkeypatch,
):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Checked before close")
    service.add_question(10, survey.survey_id, "Recommend?", "nps")
    service.open_survey(10, survey.survey_id, [101], target_type="member")

    now = 1_000
    monkeypatch.setattr(service.repo, "_now", lambda: now)
    claim = service.claim_deliveries()[0]
    now = 1_300
    snapshot = service.list_unconfirmed_deliveries(10, survey.survey_id)[0]
    assert service.mark_delivery_receipt_checked(
        snapshot.recipient_id,
        snapshot.attempt_count,
        snapshot.claimed_at,
        snapshot.delivery_status,
        snapshot.updated_at,
    ) is True

    service.close_survey(10, survey.survey_id)
    session = service.get_response_session(survey.survey_id, claim.discord_id)

    assert session is not None
    assert session.recipient.delivery_status is SurveyDeliveryStatus.FAILED
    assert session.recipient.claimed_at is None
    assert service.list_unconfirmed_deliveries(10, survey.survey_id) == []


def test_closed_survey_recovers_a_dm_receipt_before_finalizing_controls(repo_db_path):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Close after ambiguous delivery")
    service.add_question(10, survey.survey_id, "Recommend?", "nps")
    service.open_survey(10, survey.survey_id, [101], target_type="member")
    claim = service.claim_deliveries()[0]

    closed = service.close_survey(10, survey.survey_id)
    assert closed.status is SurveyStatus.CLOSED
    unconfirmed = service.list_unconfirmed_deliveries(
        10,
        survey.survey_id,
        sending_stale_after_seconds=0,
    )
    assert [recipient.recipient_id for recipient in unconfirmed] == [
        claim.recipient_id
    ]

    reconciled = service.reconcile_delivery_sent(
        claim.recipient_id,
        claim.attempt_count,
        501,
        601,
    )
    assert reconciled.delivery_status is SurveyDeliveryStatus.SENT
    sessions = service.list_recoverable_response_sessions()
    assert len(sessions) == 1
    assert sessions[0].survey.status is SurveyStatus.CLOSED
    assert sessions[0].recipient.dm_message_id == 601
    assert service.get_results(10, survey.survey_id).delivered_count == 1


def test_atomic_progress_submission_and_anonymous_submitted_only_results(repo_db_path):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "NPS")
    nps = service.add_question(10, survey.survey_id, "Recommend?", "nps")
    rating = service.add_question(
        10,
        survey.survey_id,
        "Experience?",
        "rating",
        required=False,
    )
    text = service.add_question(
        10,
        survey.survey_id,
        "Why?",
        "text",
        required=False,
    )
    _open_and_deliver(
        service,
        guild_id=10,
        survey_id=survey.survey_id,
        recipient_ids=[101, 102, 103],
    )

    first = service.answer_question(survey.survey_id, 101, nps.question_id, 10)
    assert first.recipient.current_question_id == rating.question_id
    with pytest.raises(SurveyResponseError, match="current"):
        service.answer_question(survey.survey_id, 101, nps.question_id, 9)
    service.answer_question(survey.survey_id, 101, rating.question_id, 5)
    review = service.answer_question(survey.survey_id, 101, text.question_id, "Great")
    assert review.recipient.in_review is True
    # Review revisions are allowed but stay in the review state.
    service.select_response_question(survey.survey_id, 101, nps.question_id)
    revised = service.answer_question(survey.survey_id, 101, nps.question_id, 9)
    assert revised.recipient.in_review is True
    assert revised.recipient.current_question_id is None
    assert service.submit_response(survey.survey_id, 101) is True
    assert service.submit_response(survey.survey_id, 101) is False

    # This response is deliberately left incomplete and must not affect aggregates.
    service.answer_question(survey.survey_id, 102, nps.question_id, 7)

    service.answer_question(survey.survey_id, 103, nps.question_id, 0)
    service.skip_question(survey.survey_id, 103, rating.question_id)
    service.answer_question(survey.survey_id, 103, text.question_id, "Needs work")
    assert service.submit_response(survey.survey_id, 103) is True

    results = service.get_results(10, survey.survey_id)
    assert (results.recipient_count, results.delivered_count) == (3, 3)
    assert (results.started_count, results.completed_count) == (3, 2)
    assert results.response_rate == pytest.approx(2 / 3)

    nps_result, rating_result, text_result = results.questions
    assert (
        nps_result.nps_score,
        nps_result.promoters,
        nps_result.passives,
        nps_result.detractors,
    ) == (0, 1, 0, 1)
    assert rating_result.rating_average == 5
    assert rating_result.rating_distribution == (0, 0, 0, 0, 1)
    assert rating_result.skipped_count == 1
    assert text_result.text_responses == ("Great", "Needs work")

    result_payload = asdict(results)
    assert "discord_id" not in result_payload
    assert "recipient_id" not in result_payload
    assert all("answers" not in question for question in result_payload["questions"])

    closed = service.close_survey(10, survey.survey_id)
    assert closed.status is SurveyStatus.CLOSED
    with pytest.raises(SurveyStateError, match="closed"):
        service.answer_question(survey.survey_id, 102, rating.question_id, 3)


def test_submit_is_atomic_under_duplicate_concurrent_interactions(repo_db_path):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Atomic submit")
    question = service.add_question(10, survey.survey_id, "Recommend?", "nps")
    _open_and_deliver(
        service,
        guild_id=10,
        survey_id=survey.survey_id,
        recipient_ids=[101],
    )
    service.answer_question(survey.survey_id, 101, question.question_id, 10)

    with ThreadPoolExecutor(max_workers=2) as executor:
        outcomes = list(
            executor.map(
                lambda _index: service.submit_response(survey.survey_id, 101),
                range(2),
            )
        )
    assert sorted(outcomes) == [False, True]


def test_answer_is_idempotent_under_duplicate_concurrent_interactions(repo_db_path):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Atomic answer")
    question = service.add_question(10, survey.survey_id, "Recommend?", "nps")
    _open_and_deliver(
        service,
        guild_id=10,
        survey_id=survey.survey_id,
        recipient_ids=[101],
    )

    with ThreadPoolExecutor(max_workers=2) as executor:
        sessions = list(
            executor.map(
                lambda _index: service.answer_question(
                    survey.survey_id,
                    101,
                    question.question_id,
                    10,
                ),
                range(2),
            )
        )

    assert all(session.recipient.in_review for session in sessions)
    persisted = service.get_response_session(survey.survey_id, 101)
    assert persisted is not None
    assert len(persisted.answers) == 1
    assert persisted.answers[0].numeric_value == 10


def test_review_edit_selection_persists_and_blocks_submit_until_summary(repo_db_path):
    service = _service(repo_db_path)
    survey = service.create_survey(10, "Review recovery")
    first = service.add_question(10, survey.survey_id, "Recommend?", "nps")
    second = service.add_question(10, survey.survey_id, "Why?", "text")
    foreign = service.create_survey(10, "Other survey")
    foreign_question = service.add_question(10, foreign.survey_id, "Other?", "rating")
    _open_and_deliver(
        service,
        guild_id=10,
        survey_id=survey.survey_id,
        recipient_ids=[101],
    )

    with pytest.raises(SurveyResponseError, match="Finish"):
        service.select_response_question(survey.survey_id, 101, first.question_id)
    service.answer_question(survey.survey_id, 101, first.question_id, 10)
    summary = service.answer_question(survey.survey_id, 101, second.question_id, "Great")
    assert summary.recipient.in_review is True
    assert summary.recipient.current_question_id is None
    with pytest.raises(SurveyResponseError, match="does not belong"):
        service.select_response_question(
            survey.survey_id,
            101,
            foreign_question.question_id,
        )

    editing = service.select_response_question(survey.survey_id, 101, first.question_id)
    assert editing.recipient.in_review is True
    assert editing.recipient.current_question_id == first.question_id

    restarted = SurveyService(SurveyRepository(repo_db_path))
    recovered = restarted.get_response_session(survey.survey_id, 101)
    assert recovered is not None
    assert recovered.recipient.current_question_id == first.question_id
    assert recovered.current_question == first
    with pytest.raises(SurveyResponseError, match="summary"):
        restarted.submit_response(survey.survey_id, 101)

    back_to_summary = restarted.answer_question(
        survey.survey_id,
        101,
        first.question_id,
        9,
    )
    assert back_to_summary.recipient.in_review is True
    assert back_to_summary.recipient.current_question_id is None
    assert restarted.submit_response(survey.survey_id, 101) is True

    # Submitted sessions are retained for restart-time message reconciliation.
    recoverable = restarted.list_recoverable_response_sessions()
    assert len(recoverable) == 1
    assert recoverable[0].recipient.submitted_at is not None
    assert recoverable[0].recipient.ui_message_id == 20_101
    assert restarted.finalize_response_ui(
        survey.survey_id,
        101,
        20_101,
    ) is True
    assert restarted.finalize_response_ui(
        survey.survey_id,
        101,
        20_101,
    ) is False
    assert restarted.list_recoverable_response_sessions() == []


def test_answer_membership_is_checked_in_the_atomic_repository_write(repo_db_path):
    service = _service(repo_db_path)
    first = service.create_survey(10, "First")
    own_question = service.add_question(10, first.survey_id, "Own", "nps")
    second = service.create_survey(20, "Second")
    foreign_question = service.add_question(20, second.survey_id, "Foreign", "nps")
    _open_and_deliver(
        service,
        guild_id=10,
        survey_id=first.survey_id,
        recipient_ids=[101],
    )

    with pytest.raises(SurveyResponseError, match="does not belong"):
        service.repo.save_answer(
            first.survey_id,
            101,
            foreign_question.question_id,
            numeric_value=10,
            text_value=None,
        )
    session = service.get_response_session(first.survey_id, 101)
    assert session is not None
    assert session.answers == ()
    assert session.recipient.current_question_id == own_question.question_id
