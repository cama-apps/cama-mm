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

#[derive(Clone, Debug)]
struct FakeStore {
    sessions: Vec<Session>,
    mark_result: Result<(), DeliveryStoreError>,
    sent: Vec<(i64, u32, SentMessage)>,
    reconciled: Vec<(i64, u32, SentMessage)>,
    failed: Vec<(i64, u32, &'static str)>,
    checked: Vec<ReceiptSnapshot>,
}

impl FakeStore {
    fn new(sessions: Vec<Session>) -> Self {
        Self {
            sessions,
            mark_result: Ok(()),
            sent: Vec::new(),
            reconciled: Vec::new(),
            failed: Vec::new(),
            checked: Vec::new(),
        }
    }
}

impl DeliveryStorePort for FakeStore {
    fn get_session(&mut self, _survey_id: i64, _discord_id: i64) -> Option<Session> {
        if self.sessions.len() > 1 {
            Some(self.sessions.remove(0))
        } else {
            self.sessions.first().cloned()
        }
    }

    fn mark_sent(
        &mut self,
        recipient_id: i64,
        attempt_count: u32,
        message: SentMessage,
    ) -> Result<(), DeliveryStoreError> {
        self.sent.push((recipient_id, attempt_count, message));
        self.mark_result.clone()
    }

    fn reconcile_sent(&mut self, recipient_id: i64, attempt_count: u32, message: SentMessage) {
        self.reconciled.push((recipient_id, attempt_count, message));
    }

    fn mark_failed(&mut self, recipient_id: i64, attempt_count: u32, category: &'static str) {
        self.failed.push((recipient_id, attempt_count, category));
    }

    fn receipt_checked(&mut self, snapshot: ReceiptSnapshot) {
        self.checked.push(snapshot);
    }
}

#[derive(Clone, Debug)]
struct FakeTransport {
    send_result: Result<SentMessage, DmError>,
    plans: Vec<DmPlan>,
    edits: Vec<RenderedResponse>,
}

impl Default for FakeTransport {
    fn default() -> Self {
        Self {
            send_result: Ok(SentMessage {
                channel_id: 6_001,
                message_id: 7_001,
            }),
            plans: Vec::new(),
            edits: Vec::new(),
        }
    }
}

impl DeliveryTransportPort for FakeTransport {
    fn send_dm(&mut self, _recipient: &Recipient, plan: &DmPlan) -> Result<SentMessage, DmError> {
        self.plans.push(plan.clone());
        self.send_result.clone()
    }

    fn edit_persisted(&mut self, _session: &Session, rendered: RenderedResponse) {
        self.edits.push(rendered);
    }
}

#[test]
fn test_survey_commands_are_guild_only_without_discord_visibility_gate() {
    let policies = [survey_command_policy(); 12];
    assert!(survey_command_policy().guild_only);
    assert!(
        policies
            .iter()
            .all(|policy| policy.default_permissions.is_none())
    );
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
fn test_runtime_admin_guard_blocks_service_access() {
    let denied = admin_command_gate(false);
    assert!(!denied.invoke_service);
    assert!(denied.ephemeral);
    assert!(
        denied
            .message
            .is_some_and(|message| message.contains("Admin only"))
    );
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
fn test_successful_dm_marks_delivery_sent_with_persistent_controls() {
    let mut claimed = recipient();
    claimed.delivery_status = DeliveryStatus::Sending;
    claimed.dm_message_id = None;
    let mut open = session();
    open.recipient = claimed.clone();
    let mut store = FakeStore::new(vec![open]);
    let mut transport = FakeTransport::default();
    assert_eq!(
        deliver_recipient(&mut store, &mut transport, &claimed),
        DeliveryOutcome::Sent
    );
    assert_eq!(store.sent.len(), 1);
    assert!(store.failed.is_empty());
    assert_eq!(transport.plans[0].nonce, delivery_nonce(501));
    assert!(!transport.plans[0].allowed_mentions);
    assert!(matches!(
        transport.plans[0].rendered,
        RenderedResponse::Question { .. }
    ));
}

#[test]
fn test_successful_dm_is_not_relabelled_failed_when_receipt_persistence_fails() {
    let mut claimed = recipient();
    claimed.delivery_status = DeliveryStatus::Sending;
    let mut open = session();
    open.recipient = claimed.clone();
    let mut store = FakeStore::new(vec![open]);
    store.mark_result = Err(DeliveryStoreError::Unavailable);
    let mut transport = FakeTransport::default();
    assert_eq!(
        deliver_recipient(&mut store, &mut transport, &claimed),
        DeliveryOutcome::SentReceiptUnconfirmed
    );
    assert!(store.failed.is_empty());
    assert_eq!(transport.plans[0].nonce, delivery_nonce(501));
}

#[test]
fn test_late_post_close_dm_receipt_is_attached_and_controls_removed() {
    let mut claimed = recipient();
    claimed.delivery_status = DeliveryStatus::Sending;
    let mut open = session();
    open.recipient = claimed.clone();
    let mut closed = open.clone();
    closed.survey.status = SurveyStatus::Closed;
    closed.recipient.delivery_status = DeliveryStatus::Sent;
    closed.recipient.ui_message_id = Some(7_001);
    let mut store = FakeStore::new(vec![open, closed]);
    store.mark_result = Err(DeliveryStoreError::SurveyClosed);
    let mut transport = FakeTransport::default();
    assert_eq!(
        deliver_recipient(&mut store, &mut transport, &claimed),
        DeliveryOutcome::ClosedAfterSend
    );
    assert_eq!(store.reconciled.len(), 1);
    assert!(matches!(
        transport.edits[0],
        RenderedResponse::Closed { .. }
    ));
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
fn test_failed_dm_persists_only_sanitized_failure_category() {
    let claimed = recipient();
    let mut open = session();
    open.recipient = claimed.clone();
    let mut store = FakeStore::new(vec![open]);
    let mut transport = FakeTransport {
        send_result: Err(DmError::Forbidden),
        ..FakeTransport::default()
    };
    assert_eq!(
        deliver_recipient(&mut store, &mut transport, &claimed),
        DeliveryOutcome::Failed("Forbidden")
    );
    assert_eq!(store.failed, vec![(501, 1, "Forbidden")]);
}

#[test]
fn test_interrupted_dm_keeps_claim_for_reconnect_recovery() {
    let claimed = recipient();
    let mut open = session();
    open.recipient = claimed.clone();
    let mut store = FakeStore::new(vec![open]);
    let mut transport = FakeTransport {
        send_result: Err(DmError::Cancelled),
        ..FakeTransport::default()
    };
    assert_eq!(
        deliver_recipient(&mut store, &mut transport, &claimed),
        DeliveryOutcome::Interrupted
    );
    assert!(store.sent.is_empty() && store.failed.is_empty());
}

#[test]
fn test_delivery_dispatch_is_bounded_to_five_concurrent_dms() {
    let plan = dispatch_plan(8, None);
    assert_eq!(plan.peak_concurrency, DELIVERY_CONCURRENCY);
    assert_eq!(plan.waves, vec![5, 3]);
    assert_eq!(plan.batches, vec![8, 0]);
}

#[test]
fn test_retry_requeues_only_failed_deliveries_and_dispatches() {
    let plan = retry_plan(2, 5, 6);
    assert_eq!(plan.requeued, 2);
    assert!(plan.dispatch && !plan.reconcile_views);
    assert!(plan.confirmation.contains("Retry requested for **2**"));
}

#[test]
fn test_retry_dispatches_and_reconciles_views_when_none_failed() {
    let plan = retry_plan(0, 1, 1);
    assert!(plan.dispatch);
    assert!(plan.reconcile_views);
    assert!(plan.schedule_if_unconfirmed);
}

#[test]
fn test_retry_post_intent_dispatch_failure_arms_recovery() {
    let plan = retry_plan(1, 0, 1);
    let mut recovery = RecoveryController::default();
    if plan.dispatch {
        recovery.schedule(DELIVERY_RECEIPT_GRACE_SECONDS);
    }
    assert_eq!(recovery.scheduled_delay, Some(30));
}

#[test]
fn test_response_edit_failure_schedules_persistent_recovery() {
    let outcome = committed_ui_outcome(UiMutation::Answer, RenderFailure::DiscordUnavailable);
    assert!(outcome.schedule_recovery);
    assert!(!outcome.report_error);
}

#[test]
fn test_on_ready_failure_schedules_another_recovery_pass() {
    let mut recovery = RecoveryController::default();
    recovery.schedule(DELIVERY_RECEIPT_GRACE_SECONDS);
    assert!(recovery.timer_active);
    assert_eq!(recovery.scheduled_delay, Some(30));
}

#[test]
fn test_dispatch_failure_schedules_recovery_for_committed_campaign_state() {
    let mut recovery = RecoveryController {
        background_dispatches: 1,
        ..RecoveryController::default()
    };
    recovery.schedule(DELIVERY_RECEIPT_GRACE_SECONDS);
    assert_eq!(
        (recovery.background_dispatches, recovery.scheduled_delay),
        (1, Some(30))
    );
}

#[test]
fn test_send_post_open_dispatch_failure_arms_recovery_in_background() {
    let plan = send_plan(77, 11, 1);
    let mut recovery = RecoveryController::default();
    recovery.background_dispatches += 1;
    recovery.schedule(DELIVERY_RECEIPT_GRACE_SECONDS);
    assert!(plan.open_before_dispatch && plan.confirmation.contains("opened for **1**"));
    assert_eq!(recovery.scheduled_delay, Some(30));
}

#[test]
fn test_send_responds_immediately_and_dispatches_in_background() {
    let plan = send_plan(77, 11, 2);
    assert!(plan.open_before_dispatch && plan.confirmation_before_dispatch);
    assert!(plan.confirmation.contains("opened for **2** recipients"));
    assert_eq!(plan.background_scope, (77, 11));
}

#[test]
fn test_dispatch_scopes_receipt_reconciliation_to_the_given_survey() {
    assert_eq!(
        dispatch_plan(0, Some((77, 11))).receipt_scope,
        Some((77, 11))
    );
    assert_eq!(dispatch_plan(0, None).receipt_scope, None);
}

#[test]
fn test_respond_error_swallows_expired_interaction_token() {
    let outcome = error_response_outcome(false);
    assert!(outcome.attempted);
    assert!(!outcome.propagated_transport_error);
}

#[test]
fn test_cog_unload_cancels_owned_timer_and_worker() {
    let mut recovery = RecoveryController::default();
    recovery.schedule(60);
    recovery.worker_active = true;
    recovery.background_dispatches = 2;
    recovery.unload();
    recovery.schedule(1);
    assert!(recovery.unloaded);
    assert!(!recovery.timer_active && !recovery.worker_active);
    assert_eq!(recovery.scheduled_delay, None);
    assert_eq!(recovery.background_dispatches, 0);
}

#[test]
fn test_close_serializes_delivery_and_removes_persisted_dm_controls() {
    let plan = close_plan();
    let mut closed = session();
    closed.survey.status = SurveyStatus::Closed;
    assert!(plan.defer_first && plan.close_before_reconcile && plan.remove_controls);
    assert!(matches!(
        render_response(&closed),
        Some(RenderedResponse::Closed { .. })
    ));
    assert!(plan.ephemeral);
}

#[test]
fn test_close_post_commit_reconciliation_failure_arms_recovery() {
    let plan = close_plan();
    let mut recovery = RecoveryController::default();
    if plan.close_before_reconcile {
        recovery.schedule(DELIVERY_RECEIPT_GRACE_SECONDS);
    }
    assert_eq!(recovery.scheduled_delay, Some(30));
}

#[test]
fn test_results_defers_before_loading_anonymous_report() {
    let plan = results_command_plan(900);
    let report = SurveyResults {
        survey_id: 11,
        title: "Feedback".to_owned(),
        status: SurveyStatus::Open,
        recipient_count: 0,
        delivered_count: 0,
        started_count: 0,
        completed_count: 0,
        delivery_rate: 0.0,
        started_rate: 0.0,
        completion_rate: 0.0,
        response_rate: 0.0,
        questions: Vec::new(),
    };
    assert!(plan.defer_first && plan.ephemeral);
    assert_eq!(results_pages(&report)[0].title, "Survey results — Feedback");
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
fn test_racing_answer_acknowledges_before_waiting_for_response_lock() {
    let outcome = committed_ui_outcome(UiMutation::Answer, RenderFailure::None);
    assert!(outcome.acknowledged_before_lock);
    assert_eq!(outcome.mutation, UiMutation::Answer);
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
fn test_saved_answer_is_not_reported_as_failed_when_dm_edit_fails() {
    let outcome = committed_ui_outcome(UiMutation::Answer, RenderFailure::DiscordUnavailable);
    assert!(!outcome.report_error);
    assert!(outcome.schedule_recovery);
}

#[test]
fn test_saved_skip_is_not_reported_as_failed_when_dm_edit_fails() {
    let outcome = committed_ui_outcome(UiMutation::Skip, RenderFailure::DiscordUnavailable);
    assert_eq!(outcome.mutation, UiMutation::Skip);
    assert!(!outcome.report_error);
}

#[test]
fn test_submitted_response_is_not_reported_as_failed_when_dm_edit_fails() {
    let outcome = committed_ui_outcome(UiMutation::Submit, RenderFailure::DiscordUnavailable);
    assert!(!outcome.report_error);
    assert!(outcome.schedule_recovery);
    assert!(!outcome.finalizes_controls);
}

#[test]
fn test_review_navigation_is_not_reported_as_failed_when_dm_edit_fails() {
    let edit = committed_ui_outcome(
        UiMutation::SelectQuestion,
        RenderFailure::DiscordUnavailable,
    );
    let back = committed_ui_outcome(UiMutation::BeginReview, RenderFailure::DiscordUnavailable);
    assert!(!edit.report_error && !back.report_error);
    assert!(edit.schedule_recovery && back.schedule_recovery);
}

#[test]
fn test_committed_answer_schedules_recovery_when_discord_edit_fails() {
    let outcome = committed_ui_outcome(UiMutation::Answer, RenderFailure::DiscordUnavailable);
    let mut recovery = RecoveryController::default();
    if outcome.schedule_recovery {
        recovery.schedule(DELIVERY_RECEIPT_GRACE_SECONDS);
    }
    assert_eq!(recovery.scheduled_delay, Some(30));
}

#[test]
fn test_submit_is_atomic_and_removes_all_response_controls() {
    let outcome = committed_ui_outcome(UiMutation::Submit, RenderFailure::None);
    let mut submitted = session();
    submitted.recipient.current_question_id = None;
    submitted.recipient.in_review = true;
    submitted.recipient.submitted_at = Some(100);
    assert!(outcome.finalizes_controls);
    assert!(
        matches!(render_response(&submitted), Some(RenderedResponse::Submitted { embed }) if embed.title == "Survey submitted")
    );
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
fn test_results_pagination_is_locked_to_requesting_admin() {
    let pager = ResultsPager::new(900, 2, None);
    assert!(!pager.interaction_check(901));
    assert!(pager.interaction_check(900));
}

#[test]
fn test_results_view_timeout_disables_pagination_buttons() {
    let mut pager = ResultsPager::new(900, 2, Some(1));
    pager.retire();
    assert!(pager.disabled && pager.stopped);
    assert_eq!(pager.retired_via, Some(1));
}

#[test]
fn test_results_view_timeout_edits_via_the_freshest_pagination_token() {
    let mut pager = ResultsPager::new(900, 2, Some(1));
    pager.click(2, PageDirection::Next, PageEditResult::Success);
    pager.retire();
    assert_eq!(pager.index, 1);
    assert_eq!(pager.retired_via, Some(2));
}

#[test]
fn test_results_view_failed_click_retires_via_the_previous_token() {
    let mut pager = ResultsPager::new(900, 2, Some(1));
    pager.click(2, PageDirection::Next, PageEditResult::TokenNotFound);
    assert!(pager.stopped && pager.disabled);
    assert_eq!(pager.retired_via, Some(1));
    assert_eq!(pager.edit_token, Some(1));
}

#[test]
fn test_results_view_transient_click_failure_keeps_the_view_alive() {
    let mut pager = ResultsPager::new(900, 2, Some(1));
    pager.click(2, PageDirection::Next, PageEditResult::TransientFailure);
    assert!(!pager.stopped && !pager.disabled);
    assert_eq!(pager.edit_token, Some(1));
}

#[test]
fn test_results_command_wires_timeout_interaction_into_pagination() {
    let plan = results_command_plan(900);
    let pager = ResultsPager::new(plan.owner_id, 2, Some(55));
    assert!(plan.retains_interaction);
    assert_eq!(pager.edit_token, Some(55));
    assert_eq!(pager.owner_id, 900);
}

#[test]
fn test_reconnect_reconciliation_skips_unchanged_response_messages() {
    let initial = session();
    let mut cache = ReconciliationCache::default();
    assert!(cache.should_render(&initial, 7_001));
    cache.remember(&initial, 7_001);
    assert!(!cache.should_render(&initial, 7_001));
    let mut changed = initial.clone();
    changed.recipient.current_question_id = Some(102);
    changed.recipient.updated_at = 6;
    assert!(cache.should_render(&changed, 7_001));
    cache.remember(&changed, 7_001);
    let mut closed = changed.clone();
    closed.survey.status = SurveyStatus::Closed;
    assert!(cache.should_render(&closed, 7_001));
}

#[test]
fn test_reconciliation_cache_holds_no_answer_text_and_evicts_on_finalize() {
    let secret = "my private free-text feedback";
    let mut current = session();
    current.answers.push(DraftAnswer {
        question_id: 102,
        numeric_value: None,
        text_value: Some(secret.to_owned()),
        skipped: false,
        updated_at: 5,
    });
    let mut cache = ReconciliationCache::default();
    cache.remember(&current, 7_001);
    assert_eq!(cache.len(), 1);
    assert!(!format!("{cache:?}").contains(secret));
    cache.finalize(&current);
    assert!(cache.is_empty());
}

#[test]
fn test_setup_restores_question_and_review_views_by_persisted_message_id() {
    let question_session = session();
    let mut review_session = session();
    review_session.recipient.recipient_id = 502;
    review_session.recipient.discord_id = 43;
    review_session.recipient.dm_message_id = Some(7_002);
    review_session.recipient.ui_message_id = Some(7_999);
    review_session.recipient.current_question_id = None;
    review_session.recipient.in_review = true;
    let restored = restore_response_views(&[question_session, review_session]);
    assert_eq!(restored.len(), 2);
    assert_eq!(restored[0].message_id, 7_001);
    assert_eq!(restored[1].message_id, 7_999);
    assert!(
        restored
            .iter()
            .all(|item| item.view.timeout_seconds.is_none())
    );
}

#[test]
fn test_setup_schedules_delivery_recovery_for_interrupted_campaigns() {
    let mut recovery = RecoveryController::default();
    recovery.schedule(DELIVERY_RECEIPT_GRACE_SECONDS);
    assert_eq!(recovery.scheduled_delay, Some(30));
    assert!(recovery.timer_active);
}

#[test]
fn test_ready_recovery_reconciles_stale_discord_components_to_committed_question() {
    let plan = ready_recovery_plan();
    let mut committed = session();
    committed.recipient.current_question_id = Some(102);
    committed.recipient.updated_at = 6;
    committed.answers.push(DraftAnswer {
        question_id: 101,
        numeric_value: Some(10),
        text_value: None,
        skipped: false,
        updated_at: 10,
    });
    let rendered = render_response(&committed);
    assert!(plan.reconcile_receipts_first && plan.reconcile_response_messages && plan.repeatable);
    assert_eq!(plan.recover_stale_seconds, 300);
    assert_eq!(plan.claim_batch_size, 25);
    assert!(
        matches!(rendered, Some(RenderedResponse::Question { embed, view }) if embed.footer.as_ref().is_some_and(|footer| footer.starts_with("Question 2/2")) && matches!(view.controls[0], Control::Text { .. }))
    );
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
