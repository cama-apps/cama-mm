use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use super::*;
use crate::test_support::copy_migrated_database;

fn fixture() -> (tempfile::TempDir, SurveyRepository, Arc<AtomicI64>) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("survey.db");
    copy_migrated_database(&path).expect("migrate");
    let clock = Arc::new(AtomicI64::new(1_800_000_000));
    let repository = SurveyRepository::with_clock(&path, {
        let clock = Arc::clone(&clock);
        move || clock.load(Ordering::SeqCst)
    });
    (directory, repository, clock)
}

#[test]
fn test_schema_is_guild_scoped_and_draft_questions_are_resequenced() {
    let (_directory, repository, _clock) = fixture();
    let first = repository.create_survey(10, "First guild").unwrap();
    let other = repository.create_survey(20, "Other guild").unwrap();
    let one = repository
        .add_question(10, first.survey_id, "One", SurveyQuestionType::Nps, true)
        .unwrap();
    repository
        .add_question(10, first.survey_id, "Two", SurveyQuestionType::Rating, true)
        .unwrap();
    let three = repository
        .add_question(
            10,
            first.survey_id,
            "Three",
            SurveyQuestionType::Text,
            false,
        )
        .unwrap();
    let moved = repository
        .move_question(10, first.survey_id, three.question_id, 1)
        .unwrap();
    assert_eq!(
        moved.iter().map(|row| row.position).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(
        repository
            .remove_question(10, first.survey_id, one.question_id)
            .unwrap()
    );
    let remaining = repository.get_questions(10, first.survey_id).unwrap();
    assert_eq!(
        remaining
            .iter()
            .map(|row| (row.position, row.prompt.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "Three"), (2, "Two")]
    );
    assert!(
        repository
            .get_survey(20, first.survey_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(repository.list_surveys(20, None).unwrap(), vec![other]);
}

#[test]
fn test_max_questions_and_open_campaign_are_immutable() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository.create_survey(10, "Question limit").unwrap();
    for index in 0..MAX_SURVEY_QUESTIONS {
        repository
            .add_question(
                10,
                survey.survey_id,
                &format!("Question {index}"),
                SurveyQuestionType::Rating,
                true,
            )
            .unwrap();
    }
    assert!(matches!(
        repository.add_question(
            10,
            survey.survey_id,
            "Too many",
            SurveyQuestionType::Rating,
            true
        ),
        Err(SurveyRepositoryError::State(_))
    ));
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    assert!(matches!(
        repository.edit_question(10, survey.survey_id, 1, Some("No"), None, None),
        Err(SurveyRepositoryError::State(_))
    ));
    assert!(matches!(
        repository.delete_survey(10, survey.survey_id),
        Err(SurveyRepositoryError::State(_))
    ));
}

#[test]
fn test_delivery_claim_failure_retry_and_stale_recovery() {
    let (_directory, repository, clock) = fixture();
    let survey = repository.create_survey(10, "Delivery").unwrap();
    repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101, 102],
            SurveyTargetType::Role,
            false,
        )
        .unwrap();
    let first = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_failed(first.recipient_id, first.attempt_count, "Forbidden")
        .unwrap();
    clock.fetch_add(31, Ordering::SeqCst);
    let ambiguous = repository
        .list_unconfirmed_deliveries(None, 300, 30)
        .unwrap();
    assert_eq!(ambiguous.len(), 1);
    assert!(
        repository
            .mark_delivery_receipt_checked(&ambiguous[0])
            .unwrap()
    );
    assert_eq!(
        repository
            .retry_failed_deliveries(10, survey.survey_id)
            .unwrap(),
        1
    );
    let retried = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(retried.recipient_id, retried.attempt_count, 6001, 7001)
        .unwrap();
    let second = repository.claim_deliveries(1, 300).unwrap().remove(0);
    clock.fetch_add(301, Ordering::SeqCst);
    assert_eq!(repository.recover_stale_deliveries(300).unwrap(), 0);
    assert_eq!(
        repository
            .list_unconfirmed_deliveries(None, 300, 30)
            .unwrap()
            .iter()
            .map(|row| row.recipient_id)
            .collect::<Vec<_>>(),
        vec![second.recipient_id]
    );
}

#[test]
fn test_atomic_progress_submission_and_anonymous_submitted_only_results() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository.create_survey(10, "NPS").unwrap();
    let nps = repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    let rating = repository
        .add_question(
            10,
            survey.survey_id,
            "Rate it",
            SurveyQuestionType::Rating,
            true,
        )
        .unwrap();
    let text = repository
        .add_question(
            10,
            survey.survey_id,
            "Why?",
            SurveyQuestionType::Text,
            false,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101, 102],
            SurveyTargetType::Role,
            false,
        )
        .unwrap();
    for recipient in repository.claim_deliveries(2, 300).unwrap() {
        repository
            .mark_delivery_sent(
                recipient.recipient_id,
                recipient.attempt_count,
                6_000 + recipient.discord_id,
                7_000 + recipient.discord_id,
            )
            .unwrap();
    }
    repository
        .save_answer(
            survey.survey_id,
            101,
            nps.question_id,
            SurveyAnswer::Numeric(10),
        )
        .unwrap();
    repository
        .save_answer(
            survey.survey_id,
            101,
            rating.question_id,
            SurveyAnswer::Numeric(5),
        )
        .unwrap();
    repository
        .save_answer(
            survey.survey_id,
            101,
            text.question_id,
            SurveyAnswer::Text(" Great ".to_owned()),
        )
        .unwrap();
    repository.begin_review(survey.survey_id, 101).unwrap();
    assert!(repository.submit_response(survey.survey_id, 101).unwrap());
    assert!(!repository.submit_response(survey.survey_id, 101).unwrap());

    repository
        .save_answer(
            survey.survey_id,
            102,
            nps.question_id,
            SurveyAnswer::Numeric(0),
        )
        .unwrap();
    let results = repository.get_results(10, survey.survey_id).unwrap();
    assert_eq!(results.recipient_count, 2);
    assert_eq!(results.delivered_count, 2);
    assert_eq!(results.started_count, 2);
    assert_eq!(results.completed_count, 1);
    assert_eq!(results.questions[0].nps_score, Some(100));
    assert_eq!(results.questions[1].rating_average, Some(5.0));
    assert_eq!(results.questions[2].text_responses, vec!["Great"]);
}

#[test]
fn test_submit_is_atomic_under_duplicate_concurrent_interactions() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository.create_survey(10, "Atomic submit").unwrap();
    let question = repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    let recipient = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(
            recipient.recipient_id,
            recipient.attempt_count,
            6_001,
            7_001,
        )
        .unwrap();
    repository
        .save_answer(
            survey.survey_id,
            101,
            question.question_id,
            SurveyAnswer::Numeric(10),
        )
        .unwrap();
    repository.begin_review(survey.survey_id, 101).unwrap();

    let survey_id = survey.survey_id;
    let handles = (0..8)
        .map(|_| {
            let repository = repository.clone();
            std::thread::spawn(move || repository.submit_response(survey_id, 101).unwrap())
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|submitted| **submitted).count(), 1);
}

#[test]
fn test_nps_rounding_uses_nearest_whole_with_conventional_ties() {
    assert_eq!(round_ratio_to_nearest(50, 100), 1);
    assert_eq!(round_ratio_to_nearest(-50, 100), -1);
    assert_eq!(round_ratio_to_nearest(149, 100), 1);
    assert_eq!(round_ratio_to_nearest(150, 100), 2);
}

#[test]
fn test_create_and_question_input_limits() {
    let (_directory, repository, _clock) = fixture();
    assert!(matches!(
        repository.create_survey(10, "  "),
        Err(SurveyRepositoryError::Invalid(_))
    ));
    assert!(matches!(
        repository.create_survey(10, &"x".repeat(101)),
        Err(SurveyRepositoryError::Invalid(_))
    ));
    let survey = repository.create_survey(10, " Trimmed title ").unwrap();
    assert_eq!(survey.title, "Trimmed title");
    assert!(matches!(
        repository.add_question(10, survey.survey_id, " ", SurveyQuestionType::Nps, true),
        Err(SurveyRepositoryError::Invalid(_))
    ));
    let question = repository
        .add_question(
            10,
            survey.survey_id,
            " Recommend? ",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    assert_eq!(question.prompt, "Recommend?");
}

#[test]
fn test_target_shape_and_recipient_validation() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository.create_survey(10, "Targets").unwrap();
    repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    assert!(matches!(
        repository.open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::AllRegistered,
            true
        ),
        Err(SurveyRepositoryError::Invalid(_))
    ));
    assert!(matches!(
        repository.open_survey(
            10,
            survey.survey_id,
            &[101, 102],
            SurveyTargetType::Member,
            false
        ),
        Err(SurveyRepositoryError::Invalid(_))
    ));
    assert!(matches!(
        repository.open_survey(
            10,
            survey.survey_id,
            &[-1],
            SurveyTargetType::AllRegistered,
            false
        ),
        Err(SurveyRepositoryError::Invalid(_))
    ));
}

fn answer_validation_fixture(
    question_type: SurveyQuestionType,
) -> (tempfile::TempDir, SurveyRepository, Survey, SurveyQuestion) {
    let (directory, repository, _clock) = fixture();
    let survey = repository
        .create_survey(10, &format!("Validation {}", question_type.as_str()))
        .unwrap();
    let question = repository
        .add_question(10, survey.survey_id, "Question", question_type, true)
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    let recipient = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(recipient.recipient_id, recipient.attempt_count, 501, 601)
        .unwrap();
    (directory, repository, survey, question)
}

#[test]
fn test_answer_validation_by_question_type_nps_invalid_values0_10() {
    let (_directory, repository, survey, question) =
        answer_validation_fixture(SurveyQuestionType::Nps);
    for value in [-1, 11] {
        assert!(
            repository
                .save_answer(
                    survey.survey_id,
                    101,
                    question.question_id,
                    SurveyAnswer::Numeric(value)
                )
                .is_err()
        );
    }
    let session = repository
        .save_answer(
            survey.survey_id,
            101,
            question.question_id,
            SurveyAnswer::Numeric(10),
        )
        .unwrap();
    assert!(session.recipient.in_review);
}

#[test]
fn test_answer_validation_by_question_type_rating_invalid_values1_5() {
    let (_directory, repository, survey, question) =
        answer_validation_fixture(SurveyQuestionType::Rating);
    for value in [0, 6] {
        assert!(
            repository
                .save_answer(
                    survey.survey_id,
                    101,
                    question.question_id,
                    SurveyAnswer::Numeric(value)
                )
                .is_err()
        );
    }
    let session = repository
        .save_answer(
            survey.survey_id,
            101,
            question.question_id,
            SurveyAnswer::Numeric(5),
        )
        .unwrap();
    assert!(session.recipient.in_review);
}

#[test]
fn test_answer_validation_by_question_type_text_invalid_values2_useful_feedback() {
    let (_directory, repository, survey, question) =
        answer_validation_fixture(SurveyQuestionType::Text);
    for value in [String::new(), "   ".to_owned(), "x".repeat(1_001)] {
        assert!(
            repository
                .save_answer(
                    survey.survey_id,
                    101,
                    question.question_id,
                    SurveyAnswer::Text(value)
                )
                .is_err()
        );
    }
    let session = repository
        .save_answer(
            survey.survey_id,
            101,
            question.question_id,
            SurveyAnswer::Text("Useful feedback".to_owned()),
        )
        .unwrap();
    assert!(session.recipient.in_review);
}

#[test]
fn test_required_question_cannot_be_skipped_but_optional_can() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository.create_survey(10, "Skipping").unwrap();
    let required = repository
        .add_question(
            10,
            survey.survey_id,
            "Required",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    let optional = repository
        .add_question(
            10,
            survey.survey_id,
            "Optional",
            SurveyQuestionType::Text,
            false,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    let recipient = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(recipient.recipient_id, recipient.attempt_count, 501, 601)
        .unwrap();
    assert!(matches!(
        repository.skip_question(survey.survey_id, 101, required.question_id),
        Err(SurveyRepositoryError::Response(_))
    ));
    repository
        .save_answer(
            survey.survey_id,
            101,
            required.question_id,
            SurveyAnswer::Numeric(8),
        )
        .unwrap();
    let session = repository
        .skip_question(survey.survey_id, 101, optional.question_id)
        .unwrap();
    assert!(session.recipient.in_review);
    assert!(session.answers.last().unwrap().skipped);
    let duplicate = repository
        .skip_question(survey.survey_id, 101, optional.question_id)
        .unwrap();
    assert_eq!(duplicate.answers.len(), 2);
}

#[test]
fn test_unchecked_legacy_pending_attempt_cannot_be_claimed() {
    let (_directory, repository, clock) = fixture();
    let survey = repository.create_survey(10, "Legacy pending").unwrap();
    repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    clock.store(1_000, Ordering::SeqCst);
    let first = repository.claim_deliveries(1, 300).unwrap().remove(0);
    let connection = repository.connection().unwrap();
    connection
        .execute(
            "UPDATE survey_recipients SET delivery_status='pending',
             receipt_checked_attempt=0,claimed_at=NULL,updated_at=1
             WHERE recipient_id=?1",
            [first.recipient_id],
        )
        .unwrap();
    assert!(repository.claim_deliveries(25, 300).unwrap().is_empty());
    let unconfirmed = repository
        .list_unconfirmed_deliveries(Some((10, survey.survey_id)), 300, 0)
        .unwrap();
    assert_eq!(unconfirmed[0].recipient_id, first.recipient_id);
}

#[test]
fn test_retry_intent_waits_for_receipt_grace_then_queues_automatically() {
    let (_directory, repository, clock) = fixture();
    let survey = repository.create_survey(10, "Durable retry").unwrap();
    repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    clock.store(1_000, Ordering::SeqCst);
    let claim = repository.claim_deliveries(25, 300).unwrap().remove(0);
    repository
        .mark_delivery_failed(claim.recipient_id, claim.attempt_count, "Forbidden")
        .unwrap();
    assert_eq!(
        repository
            .retry_failed_deliveries(10, survey.survey_id)
            .unwrap(),
        1
    );
    assert!(repository.claim_deliveries(25, 300).unwrap().is_empty());
    clock.store(1_029, Ordering::SeqCst);
    assert!(
        repository
            .list_unconfirmed_deliveries(Some((10, survey.survey_id)), 300, 30)
            .unwrap()
            .is_empty()
    );
    clock.store(1_030, Ordering::SeqCst);
    let eligible = repository
        .list_unconfirmed_deliveries(Some((10, survey.survey_id)), 300, 30)
        .unwrap();
    assert_eq!(eligible.len(), 1);
    assert!(
        repository
            .mark_delivery_receipt_checked(&eligible[0])
            .unwrap()
    );
    assert_eq!(repository.queue_requested_delivery_retries().unwrap(), 1);
    let retried = repository.claim_deliveries(25, 300).unwrap().remove(0);
    assert_eq!(retried.recipient_id, claim.recipient_id);
    assert_eq!(retried.attempt_count, 2);
}

#[test]
fn test_close_clears_retry_intent_and_closed_campaign_cannot_be_queued() {
    let (_directory, repository, clock) = fixture();
    let survey = repository
        .create_survey(10, "Close requested retry")
        .unwrap();
    repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    clock.store(1_000, Ordering::SeqCst);
    let claim = repository.claim_deliveries(25, 300).unwrap().remove(0);
    repository
        .mark_delivery_failed(claim.recipient_id, claim.attempt_count, "Forbidden")
        .unwrap();
    assert_eq!(
        repository
            .retry_failed_deliveries(10, survey.survey_id)
            .unwrap(),
        1
    );
    repository.close_survey(10, survey.survey_id).unwrap();
    clock.store(1_030, Ordering::SeqCst);
    let failed = repository
        .list_unconfirmed_deliveries(Some((10, survey.survey_id)), 300, 30)
        .unwrap();
    assert!(
        repository
            .mark_delivery_receipt_checked(&failed[0])
            .unwrap()
    );
    assert_eq!(repository.queue_requested_delivery_retries().unwrap(), 0);
    assert!(repository.claim_deliveries(25, 300).unwrap().is_empty());
}

#[test]
fn test_close_atomically_fails_a_receipt_checked_sending_attempt() {
    let (_directory, repository, clock) = fixture();
    let survey = repository
        .create_survey(10, "Checked before close")
        .unwrap();
    repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    clock.store(1_000, Ordering::SeqCst);
    let claim = repository.claim_deliveries(25, 300).unwrap().remove(0);
    clock.store(1_300, Ordering::SeqCst);
    let snapshot = repository
        .list_unconfirmed_deliveries(Some((10, survey.survey_id)), 300, 30)
        .unwrap();
    assert!(
        repository
            .mark_delivery_receipt_checked(&snapshot[0])
            .unwrap()
    );
    repository.close_survey(10, survey.survey_id).unwrap();
    let session = repository
        .get_response_session(survey.survey_id, claim.discord_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        session.recipient.delivery_status,
        SurveyDeliveryStatus::Failed
    );
    assert_eq!(session.recipient.claimed_at, None);
    assert!(
        repository
            .list_unconfirmed_deliveries(Some((10, survey.survey_id)), 300, 30)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_closed_survey_recovers_a_dm_receipt_before_finalizing_controls() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository
        .create_survey(10, "Close after ambiguous delivery")
        .unwrap();
    repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    let claim = repository.claim_deliveries(25, 300).unwrap().remove(0);
    assert_eq!(
        repository
            .close_survey(10, survey.survey_id)
            .unwrap()
            .status,
        SurveyStatus::Closed
    );
    let unconfirmed = repository
        .list_unconfirmed_deliveries(Some((10, survey.survey_id)), 0, 30)
        .unwrap();
    assert_eq!(unconfirmed[0].recipient_id, claim.recipient_id);
    let reconciled = repository
        .reconcile_delivery_sent(claim.recipient_id, claim.attempt_count, 501, 601)
        .unwrap();
    assert_eq!(reconciled.delivery_status, SurveyDeliveryStatus::Sent);
    let sessions = repository.list_recoverable_response_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].survey.status, SurveyStatus::Closed);
    assert_eq!(sessions[0].recipient.dm_message_id, Some(601));
    assert_eq!(
        repository
            .get_results(10, survey.survey_id)
            .unwrap()
            .delivered_count,
        1
    );
}

#[test]
fn test_answer_is_idempotent_under_duplicate_concurrent_interactions() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository.create_survey(10, "Atomic answer").unwrap();
    let question = repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    let recipient = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(recipient.recipient_id, recipient.attempt_count, 501, 601)
        .unwrap();
    let survey_id = survey.survey_id;
    let question_id = question.question_id;
    let handles = (0..2)
        .map(|_| {
            let repository = repository.clone();
            std::thread::spawn(move || {
                repository
                    .save_answer(survey_id, 101, question_id, SurveyAnswer::Numeric(10))
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    assert!(
        handles
            .into_iter()
            .all(|handle| handle.join().unwrap().recipient.in_review)
    );
    let persisted = repository
        .get_response_session(survey_id, 101)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.answers.len(), 1);
    assert_eq!(persisted.answers[0].numeric_value, Some(10));
}

#[test]
fn test_review_edit_selection_persists_and_blocks_submit_until_summary() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository.create_survey(10, "Review recovery").unwrap();
    let first = repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    let second = repository
        .add_question(10, survey.survey_id, "Why?", SurveyQuestionType::Text, true)
        .unwrap();
    let foreign = repository.create_survey(10, "Other survey").unwrap();
    let foreign_question = repository
        .add_question(
            10,
            foreign.survey_id,
            "Other?",
            SurveyQuestionType::Rating,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    let recipient = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(
            recipient.recipient_id,
            recipient.attempt_count,
            10_101,
            20_101,
        )
        .unwrap();
    assert!(
        repository
            .select_response_question(survey.survey_id, 101, first.question_id)
            .is_err()
    );
    repository
        .save_answer(
            survey.survey_id,
            101,
            first.question_id,
            SurveyAnswer::Numeric(10),
        )
        .unwrap();
    repository
        .save_answer(
            survey.survey_id,
            101,
            second.question_id,
            SurveyAnswer::Text("Great".to_owned()),
        )
        .unwrap();
    assert!(
        repository
            .select_response_question(survey.survey_id, 101, foreign_question.question_id)
            .is_err()
    );
    let editing = repository
        .select_response_question(survey.survey_id, 101, first.question_id)
        .unwrap();
    assert_eq!(
        editing.recipient.current_question_id,
        Some(first.question_id)
    );
    assert!(repository.submit_response(survey.survey_id, 101).is_err());
    let back = repository
        .save_answer(
            survey.survey_id,
            101,
            first.question_id,
            SurveyAnswer::Numeric(9),
        )
        .unwrap();
    assert_eq!(back.recipient.current_question_id, None);
    assert!(repository.submit_response(survey.survey_id, 101).unwrap());
    assert_eq!(
        repository
            .list_recoverable_response_sessions()
            .unwrap()
            .len(),
        1
    );
    assert!(
        repository
            .finalize_response_ui(survey.survey_id, 101, 20_101)
            .unwrap()
    );
    assert!(
        !repository
            .finalize_response_ui(survey.survey_id, 101, 20_101)
            .unwrap()
    );
    assert!(
        repository
            .list_recoverable_response_sessions()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_answer_membership_is_checked_in_the_atomic_repository_write() {
    let (_directory, repository, _clock) = fixture();
    let first = repository.create_survey(10, "First").unwrap();
    let own = repository
        .add_question(10, first.survey_id, "Own", SurveyQuestionType::Nps, true)
        .unwrap();
    let second = repository.create_survey(20, "Second").unwrap();
    let foreign = repository
        .add_question(
            20,
            second.survey_id,
            "Foreign",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(10, first.survey_id, &[101], SurveyTargetType::Member, false)
        .unwrap();
    let recipient = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(recipient.recipient_id, recipient.attempt_count, 501, 601)
        .unwrap();
    assert!(
        repository
            .save_answer(
                first.survey_id,
                101,
                foreign.question_id,
                SurveyAnswer::Numeric(10)
            )
            .is_err()
    );
    let session = repository
        .get_response_session(first.survey_id, 101)
        .unwrap()
        .unwrap();
    assert!(session.answers.is_empty());
    assert_eq!(session.recipient.current_question_id, Some(own.question_id));
}

#[test]
fn test_response_session_reads_recipient_and_answers_from_one_snapshot() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository.create_survey(10, "Session snapshot").unwrap();
    let first = repository
        .add_question(10, survey.survey_id, "First", SurveyQuestionType::Nps, true)
        .unwrap();
    repository
        .add_question(
            10,
            survey.survey_id,
            "Second",
            SurveyQuestionType::Rating,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    let recipient = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(recipient.recipient_id, recipient.attempt_count, 501, 601)
        .unwrap();
    let connection = repository.connection().unwrap();
    connection.execute_batch("BEGIN").unwrap();
    let snapshot = query_session_with_hook(&connection, survey.survey_id, 101, || {
        repository
            .save_answer(
                survey.survey_id,
                101,
                first.question_id,
                SurveyAnswer::Numeric(10),
            )
            .unwrap();
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        snapshot.recipient.current_question_id,
        Some(first.question_id)
    );
    assert!(snapshot.answers.is_empty());
}

#[test]
fn test_open_results_metrics_and_answers_share_one_snapshot() {
    let (_directory, repository, _clock) = fixture();
    let survey = repository.create_survey(10, "Results snapshot").unwrap();
    let question = repository
        .add_question(
            10,
            survey.survey_id,
            "Recommend?",
            SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            10,
            survey.survey_id,
            &[101],
            SurveyTargetType::Member,
            false,
        )
        .unwrap();
    let recipient = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(recipient.recipient_id, recipient.attempt_count, 501, 601)
        .unwrap();
    repository
        .save_answer(
            survey.survey_id,
            101,
            question.question_id,
            SurveyAnswer::Numeric(10),
        )
        .unwrap();
    let snapshot = repository
        .get_results_with_hook(10, survey.survey_id, || {
            assert!(repository.submit_response(survey.survey_id, 101).unwrap());
        })
        .unwrap();
    assert_eq!(snapshot.completed_count, 0);
    assert_eq!(snapshot.questions[0].response_count, 0);
}
