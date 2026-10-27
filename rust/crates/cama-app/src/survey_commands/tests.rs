use super::*;

fn survey(status: SurveyStatus, target_type: Option<SurveyTargetType>) -> Survey {
    Survey {
        survey_id: 11,
        guild_id: 77,
        title: "Cama feedback".to_owned(),
        status,
        target_type,
    }
}

fn question(
    question_id: i64,
    position: usize,
    question_type: QuestionType,
    required: bool,
) -> Question {
    Question {
        question_id,
        position,
        prompt: format!("Question {position}?"),
        question_type,
        required,
    }
}

fn recipient() -> Recipient {
    Recipient {
        recipient_id: 501,
        survey_id: 11,
        guild_id: 77,
        discord_id: 42,
        delivery_status: DeliveryStatus::Sent,
        attempt_count: 1,
        claimed_at: None,
        dm_channel_id: Some(6_001),
        dm_message_id: Some(7_001),
        ui_message_id: None,
        current_question_id: Some(101),
        in_review: false,
        submitted_at: None,
        updated_at: 5,
    }
}

fn session() -> Session {
    Session {
        survey: survey(SurveyStatus::Open, None),
        recipient: recipient(),
        questions: vec![
            question(101, 1, QuestionType::Nps, true),
            question(102, 2, QuestionType::Text, false),
        ],
        answers: Vec::new(),
    }
}

#[test]
fn test_delivery_history_match_uses_persistent_component_ids_only() {
    let component_message = HistoryMessage {
        message_id: 7_001,
        author_id: 999,
        nonce: None,
        components: vec![MessageComponent {
            custom_id: Some("survey:11:question:101:score".to_owned()),
        }],
    };
    let nonce_only = HistoryMessage {
        nonce: Some(delivery_nonce(501)),
        components: Vec::new(),
        ..component_message.clone()
    };
    assert!(message_matches_delivery(&component_message, 11));
    assert!(!message_matches_delivery(&component_message, 12));
    assert!(!message_matches_delivery(&nonce_only, 11));
}

#[test]
fn test_all_registered_filters_bots_fake_players_and_departed_players() {
    let guild = GuildSnapshot {
        guild_id: 77,
        members: vec![
            MemberSnapshot {
                discord_id: 1,
                guild_id: 77,
                bot: false,
            },
            MemberSnapshot {
                discord_id: 2,
                guild_id: 77,
                bot: true,
            },
            MemberSnapshot {
                discord_id: 3,
                guild_id: 77,
                bot: false,
            },
        ],
    };
    let resolved = resolve_recipients(
        &guild,
        RecipientTarget::AllRegistered,
        &[1, 2, 4, -1],
        false,
    );
    assert_eq!(resolved, Ok(vec![1]));
}

#[test]
fn test_member_target_allows_unregistered_unless_filter_requested() {
    let target = MemberSnapshot {
        discord_id: 3,
        guild_id: 77,
        bot: false,
    };
    let guild = GuildSnapshot {
        guild_id: 77,
        members: vec![target.clone()],
    };
    assert_eq!(
        resolve_recipients(&guild, RecipientTarget::Member(&target), &[], false),
        Ok(vec![3])
    );
    assert!(
        resolve_recipients(&guild, RecipientTarget::Member(&target), &[], true)
            .is_err_and(|error| error.0.contains("not a registered"))
    );
}

#[test]
fn test_member_target_rejects_bots_and_members_from_another_guild() {
    let current = MemberSnapshot {
        discord_id: 3,
        guild_id: 77,
        bot: false,
    };
    let guild = GuildSnapshot {
        guild_id: 77,
        members: vec![current],
    };
    let bot = MemberSnapshot {
        discord_id: 2,
        guild_id: 77,
        bot: true,
    };
    let other = MemberSnapshot {
        discord_id: 3,
        guild_id: 88,
        bot: false,
    };
    let departed = MemberSnapshot {
        discord_id: 4,
        guild_id: 77,
        bot: false,
    };
    assert!(
        resolve_recipients(&guild, RecipientTarget::Member(&bot), &[], false)
            .is_err_and(|error| error.0.contains("Bots"))
    );
    assert!(
        resolve_recipients(&guild, RecipientTarget::Member(&other), &[], false)
            .is_err_and(|error| error.0.contains("not in this server"))
    );
    assert!(resolve_recipients(&guild, RecipientTarget::Member(&departed), &[], false).is_err());
}

#[test]
fn test_role_target_filters_bots_and_honors_registered_only() {
    let registered = MemberSnapshot {
        discord_id: 1,
        guild_id: 77,
        bot: false,
    };
    let unregistered = MemberSnapshot {
        discord_id: 3,
        guild_id: 77,
        bot: false,
    };
    let bot = MemberSnapshot {
        discord_id: 2,
        guild_id: 77,
        bot: true,
    };
    let departed = MemberSnapshot {
        discord_id: 4,
        guild_id: 77,
        bot: false,
    };
    let guild = GuildSnapshot {
        guild_id: 77,
        members: vec![registered.clone(), unregistered.clone(), bot.clone()],
    };
    let role = RoleSnapshot {
        guild_id: 77,
        members: vec![registered, unregistered, bot, departed],
    };
    assert_eq!(
        resolve_recipients(&guild, RecipientTarget::Role(&role), &[1], false),
        Ok(vec![1, 3])
    );
    assert_eq!(
        resolve_recipients(&guild, RecipientTarget::Role(&role), &[1], true),
        Ok(vec![1])
    );
}

#[test]
fn test_recipient_resolution_filters_fakes_bots_departed_and_registration() {
    let registered = MemberSnapshot {
        discord_id: 1,
        guild_id: 55,
        bot: false,
    };
    let unregistered = MemberSnapshot {
        discord_id: 2,
        guild_id: 55,
        bot: false,
    };
    let bot = MemberSnapshot {
        discord_id: 4,
        guild_id: 55,
        bot: true,
    };
    let guild = GuildSnapshot {
        guild_id: 55,
        members: vec![registered.clone(), unregistered.clone(), bot.clone()],
    };
    assert_eq!(
        resolve_recipients(
            &guild,
            RecipientTarget::AllRegistered,
            &[1, -1, 3, 4],
            false
        ),
        Ok(vec![1])
    );
    let role = RoleSnapshot {
        guild_id: 55,
        members: vec![registered, unregistered.clone(), bot],
    };
    assert_eq!(
        resolve_recipients(&guild, RecipientTarget::Role(&role), &[1], false),
        Ok(vec![1, 2])
    );
    assert_eq!(
        resolve_recipients(&guild, RecipientTarget::Role(&role), &[1], true),
        Ok(vec![1])
    );
    assert!(
        resolve_recipients(&guild, RecipientTarget::Member(&unregistered), &[1], true)
            .is_err_and(|error| error.0.contains("not a registered"))
    );
}

#[test]
fn test_delayed_recovery_finds_existing_dm_in_history_without_resending() {
    let message = HistoryMessage {
        message_id: 7_001,
        author_id: 999,
        nonce: None,
        components: vec![MessageComponent {
            custom_id: Some("survey:11:question:101:score".to_owned()),
        }],
    };
    assert_eq!(
        reconcile_delivery_receipt(&recipient(), 999, 6_001, &[message]),
        ReceiptReconciliation::Found(SentMessage {
            channel_id: 6_001,
            message_id: 7_001
        })
    );
}

#[test]
fn test_no_history_receipt_uses_the_exact_delivery_snapshot_cas() {
    let mut claimed = recipient();
    claimed.delivery_status = DeliveryStatus::Sending;
    claimed.claimed_at = Some(4);
    claimed.updated_at = 5;
    assert_eq!(
        reconcile_delivery_receipt(&claimed, 999, 6_001, &[]),
        ReceiptReconciliation::Checked(ReceiptSnapshot {
            recipient_id: 501,
            attempt_count: 1,
            claimed_at: Some(4),
            delivery_status: DeliveryStatus::Sending,
            updated_at: 5,
        })
    );
}

#[test]
fn test_question_views_match_question_type_and_optional_skip_behavior() {
    let questions = vec![
        question(101, 1, QuestionType::Nps, true),
        question(102, 2, QuestionType::Rating, false),
        question(103, 3, QuestionType::Text, false),
    ];
    let mut base = session();
    base.questions = questions.clone();
    let nps = question_view(&base, &questions[0]);
    let rating = question_view(&base, &questions[1]);
    let text = question_view(&base, &questions[2]);
    assert!(
        matches!(&nps.controls[0], Control::Score { values, .. } if values == &(0..=10).collect::<Vec<_>>())
    );
    assert!(
        !nps.controls
            .iter()
            .any(|control| matches!(control, Control::Skip { .. }))
    );
    assert!(
        matches!(&rating.controls[0], Control::Score { values, .. } if values == &(1..=5).collect::<Vec<_>>())
    );
    assert!(
        rating
            .controls
            .iter()
            .any(|control| matches!(control, Control::Skip { .. }))
    );
    assert!(matches!(text.controls[0], Control::Text { .. }));
    assert!(text.timeout_seconds.is_none());
}

#[test]
fn test_review_question_picker_escapes_stored_prompts() {
    let mut review = session();
    review.recipient.current_question_id = None;
    review.recipient.in_review = true;
    review.questions = vec![Question {
        prompt: "Tell @everyone *why*".to_owned(),
        ..question(101, 1, QuestionType::Text, true)
    }];
    let view = review_view(&review);
    let Control::Edit { options, .. } = &view.controls[0] else {
        panic!("edit control")
    };
    assert!(!options[0].description.contains("@everyone"));
    assert!(options[0].description.contains("@\u{200b}everyone"));
    assert!(options[0].description.contains("\\*why\\*"));
}

#[test]
fn test_response_views_reject_a_different_discord_user_survey_question_view() {
    let current = session();
    let view = question_view(&current, &current.questions[0]);
    assert!(!view.interaction_check(999));
    assert!(view.interaction_check(42));
}

#[test]
fn test_response_views_reject_a_different_discord_user_survey_review_view() {
    let current = session();
    let view = review_view(&current);
    assert!(!view.interaction_check(999));
    assert!(view.interaction_check(42));
}

#[test]
fn test_answer_progresses_to_next_question_then_final_review() {
    let mut next = session();
    next.recipient.current_question_id = Some(102);
    let mut review = next.clone();
    review.recipient.current_question_id = None;
    review.recipient.in_review = true;
    review.answers = vec![
        DraftAnswer {
            question_id: 101,
            numeric_value: Some(9),
            text_value: None,
            skipped: false,
            updated_at: 10,
        },
        DraftAnswer {
            question_id: 102,
            numeric_value: None,
            text_value: None,
            skipped: true,
            updated_at: 11,
        },
    ];
    assert!(
        matches!(render_response(&next), Some(RenderedResponse::Question { view, .. }) if matches!(view.controls[0], Control::Text { .. }))
    );
    assert!(matches!(
        render_response(&review),
        Some(RenderedResponse::Review { .. })
    ));
}

#[test]
fn test_member_targeted_question_dm_does_not_promise_anonymity() {
    let mut member = session();
    member.survey.target_type = Some(SurveyTargetType::Member);
    let mut role = member.clone();
    role.survey.target_type = Some(SurveyTargetType::Role);
    let member_privacy = question_embed(&member, &member.questions[0]).fields[1]
        .value
        .clone();
    let role_privacy = question_embed(&role, &role.questions[0]).fields[1]
        .value
        .clone();
    assert!(member_privacy.contains("shared with the survey admins"));
    assert!(member_privacy.contains("not anonymous"));
    assert!(!member_privacy.contains("anonymous, per-question results"));
    assert!(role_privacy.contains("anonymous, per-question results"));
    assert!(!submitted_embed(&member).description.contains("anonymously"));
    assert!(submitted_embed(&role).description.contains("anonymously"));
}

#[test]
fn test_results_pages_show_aggregates_and_unattributed_comments_without_identity_data() {
    let results = SurveyResults {
        survey_id: 11,
        title: "Feedback @everyone".to_owned(),
        status: SurveyStatus::Closed,
        recipient_count: 6,
        delivered_count: 5,
        started_count: 4,
        completed_count: 3,
        delivery_rate: 5.0 / 6.0,
        started_rate: 4.0 / 5.0,
        completion_rate: 3.0 / 4.0,
        response_rate: 3.0 / 5.0,
        questions: vec![
            QuestionResult {
                question_id: 101,
                position: 1,
                prompt: "Would you recommend Cama?".to_owned(),
                question_type: QuestionType::Nps,
                response_count: 3,
                skipped_count: 0,
                nps_score: Some(33),
                promoters: 2,
                passives: 0,
                detractors: 1,
                rating_average: None,
                rating_distribution: [0; 5],
                text_responses: Vec::new(),
            },
            QuestionResult {
                question_id: 102,
                position: 2,
                prompt: "Rate match quality".to_owned(),
                question_type: QuestionType::Rating,
                response_count: 3,
                skipped_count: 0,
                nps_score: None,
                promoters: 0,
                passives: 0,
                detractors: 0,
                rating_average: Some(4.0),
                rating_distribution: [0, 0, 1, 1, 1],
                text_responses: Vec::new(),
            },
            QuestionResult {
                question_id: 103,
                position: 3,
                prompt: "What should change?".to_owned(),
                question_type: QuestionType::Text,
                response_count: 2,
                skipped_count: 1,
                nps_score: None,
                promoters: 0,
                passives: 0,
                detractors: 0,
                rating_average: None,
                rating_distribution: [0; 5],
                text_responses: vec!["More captains".to_owned(), "No @everyone pings".to_owned()],
            },
        ],
    };
    let pages = results_pages(&results);
    let rendered = format!("{pages:?}");
    let visible_text = pages
        .iter()
        .flat_map(|page| {
            std::iter::once(page.title.as_str())
                .chain(std::iter::once(page.description.as_str()))
                .chain(page.fields.iter().map(|field| field.value.as_str()))
        })
        .collect::<String>();
    assert_eq!(pages.len(), 4);
    assert!(rendered.contains("33") && rendered.contains("Promoters"));
    assert!(rendered.contains("4.00") && rendered.contains("1: 0") && rendered.contains("5: 1"));
    assert!(rendered.contains("More captains") && rendered.contains("Anonymous response"));
    assert!(visible_text.contains("@\u{200b}everyone"));
    assert!(!rendered.contains("discord_id") && !rendered.contains("<@"));
}

#[test]
fn test_result_pages_are_anonymous_safe_and_have_no_small_cohort_warning() {
    let results = SurveyResults {
        survey_id: 9,
        title: "Feedback @everyone".to_owned(),
        status: SurveyStatus::Open,
        recipient_count: 1,
        delivered_count: 1,
        started_count: 1,
        completed_count: 1,
        delivery_rate: 1.0,
        started_rate: 1.0,
        completion_rate: 1.0,
        response_rate: 1.0,
        questions: vec![QuestionResult {
            question_id: 10,
            position: 1,
            prompt: "Why?".to_owned(),
            question_type: QuestionType::Text,
            response_count: 1,
            skipped_count: 0,
            nps_score: None,
            promoters: 0,
            passives: 0,
            detractors: 0,
            rating_average: None,
            rating_distribution: [0; 5],
            text_responses: vec!["Because @admins are helpful".to_owned()],
        }],
    };
    let pages = results_pages(&results);
    let rendered = format!("{pages:?}").to_lowercase();
    let visible_text = pages
        .iter()
        .flat_map(|page| {
            std::iter::once(page.title.as_str())
                .chain(std::iter::once(page.description.as_str()))
                .chain(page.fields.iter().map(|field| field.value.as_str()))
        })
        .collect::<String>();
    assert!(!rendered.contains("discord_id"));
    assert!(!rendered.contains("recipient_id"));
    assert!(!rendered.contains("deanonym"));
    assert!(!rendered.contains("fewer than five"));
    assert!(!rendered.contains("small cohort"));
    assert!(!visible_text.contains("@admins"));
    assert!(visible_text.contains("@\u{200b}admins"));
    assert!(pages.iter().all(Embed::discord_safe));
}

#[test]
fn test_zero_submission_report_has_real_questions_and_zero_aggregates() {
    let results = SurveyResults {
        survey_id: 9,
        title: "No responses yet".to_owned(),
        status: SurveyStatus::Open,
        recipient_count: 1,
        delivered_count: 1,
        started_count: 0,
        completed_count: 0,
        delivery_rate: 1.0,
        started_rate: 0.0,
        completion_rate: 0.0,
        response_rate: 0.0,
        questions: vec![
            QuestionResult {
                question_id: 10,
                position: 1,
                prompt: "Recommend?".to_owned(),
                question_type: QuestionType::Nps,
                response_count: 0,
                skipped_count: 0,
                nps_score: None,
                promoters: 0,
                passives: 0,
                detractors: 0,
                rating_average: None,
                rating_distribution: [0; 5],
                text_responses: Vec::new(),
            },
            QuestionResult {
                question_id: 11,
                position: 2,
                prompt: "Rate it".to_owned(),
                question_type: QuestionType::Rating,
                response_count: 0,
                skipped_count: 0,
                nps_score: None,
                promoters: 0,
                passives: 0,
                detractors: 0,
                rating_average: None,
                rating_distribution: [0; 5],
                text_responses: Vec::new(),
            },
            QuestionResult {
                question_id: 12,
                position: 3,
                prompt: "Why?".to_owned(),
                question_type: QuestionType::Text,
                response_count: 0,
                skipped_count: 0,
                nps_score: None,
                promoters: 0,
                passives: 0,
                detractors: 0,
                rating_average: None,
                rating_distribution: [0; 5],
                text_responses: Vec::new(),
            },
        ],
    };
    let pages = results_pages(&results);
    let rendered = format!("{pages:?}");
    assert!(rendered.matches("N/A").count() >= 2);
    assert!(rendered.contains("No written responses"));
    assert!(pages.iter().all(Embed::discord_safe));
}

#[test]
fn test_result_pages_preserve_complete_max_length_escaped_text_answer() {
    let answer = "@*".repeat(500);
    let results = SurveyResults {
        survey_id: 9,
        title: "Feedback".to_owned(),
        status: SurveyStatus::Closed,
        recipient_count: 1,
        delivered_count: 1,
        started_count: 1,
        completed_count: 1,
        delivery_rate: 1.0,
        started_rate: 1.0,
        completion_rate: 1.0,
        response_rate: 1.0,
        questions: vec![QuestionResult {
            question_id: 10,
            position: 1,
            prompt: "*".repeat(1_000),
            question_type: QuestionType::Text,
            response_count: 1,
            skipped_count: 0,
            nps_score: None,
            promoters: 0,
            passives: 0,
            detractors: 0,
            rating_average: None,
            rating_distribution: [0; 5],
            text_responses: vec![answer.clone()],
        }],
    };
    let pages = results_pages(&results);
    let rendered_answer = pages[1..]
        .iter()
        .flat_map(|page| page.fields.iter())
        .map(|field| field.value.as_str())
        .collect::<String>();
    assert_eq!(rendered_answer, escape_discord_text(&answer));
    assert!(pages.iter().all(Embed::discord_safe));
}

#[test]
fn test_maximum_size_review_and_preview_embeds_stay_within_discord_limits() {
    let mut maximum = session();
    maximum.survey.title = "*".repeat(100);
    maximum.questions = (1..=10)
        .map(|position| Question {
            question_id: 100 + i64::from(position),
            position: position as usize,
            prompt: "*".repeat(1_000),
            question_type: QuestionType::Text,
            required: true,
        })
        .collect();
    maximum.answers = maximum
        .questions
        .iter()
        .map(|item| DraftAnswer {
            question_id: item.question_id,
            numeric_value: None,
            text_value: Some("*".repeat(1_000)),
            skipped: false,
            updated_at: 10,
        })
        .collect();
    maximum.recipient.current_question_id = None;
    maximum.recipient.in_review = true;
    let review = review_embed(&maximum);
    let previews = preview_pages(&maximum.survey, &maximum.questions);
    let rendered_prompts = previews
        .iter()
        .flat_map(|page| page.fields.iter())
        .map(|field| field.value.as_str())
        .collect::<String>();
    assert_eq!(review.fields.len(), 10);
    assert!(review.discord_safe());
    assert_eq!(
        rendered_prompts,
        escape_discord_text(&"*".repeat(1_000)).repeat(10)
    );
    assert!(previews.iter().all(Embed::discord_safe));
}

#[test]
fn results_pages_keep_long_written_responses_inside_the_embed_budget() {
    // Written answers run to 1000 characters and escaping roughly doubles them,
    // so a fixed four-responses-per-page split overruns Discord's 6000-char
    // embed budget and the whole results followup is rejected.
    let long_answer = "a. ".repeat(330);
    let results = SurveyResults {
        survey_id: 12,
        title: "Long answers".to_owned(),
        status: SurveyStatus::Closed,
        recipient_count: 12,
        delivered_count: 12,
        started_count: 12,
        completed_count: 12,
        delivery_rate: 1.0,
        started_rate: 1.0,
        completion_rate: 1.0,
        response_rate: 1.0,
        questions: vec![QuestionResult {
            question_id: 201,
            position: 1,
            prompt: "Tell us everything".to_owned(),
            question_type: QuestionType::Text,
            response_count: 12,
            skipped_count: 0,
            nps_score: None,
            promoters: 0,
            passives: 0,
            detractors: 0,
            rating_average: None,
            rating_distribution: [0; 5],
            text_responses: vec![long_answer; 12],
        }],
    };

    let pages = results_pages(&results);

    assert!(pages.len() > 1, "long answers must span multiple pages");
    for (index, page) in pages.iter().enumerate() {
        assert!(
            page.discord_safe(),
            "page {index} exceeds Discord's embed limits: {} chars, {} fields",
            page.content_len(),
            page.fields.len()
        );
    }
    let rendered = pages
        .iter()
        .flat_map(|page| page.fields.iter())
        .filter(|field| field.name.starts_with("Anonymous response"))
        .count();
    assert!(
        rendered >= 12,
        "every response is still rendered, got {rendered} response fields"
    );
}
