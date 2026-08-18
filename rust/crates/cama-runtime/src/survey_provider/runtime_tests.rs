use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::test_support::initialize_test_database as initialize_or_migrate;
use async_trait::async_trait;
use tokio::sync::Semaphore;

use super::*;
use crate::gateway_events::{GatewayMember, GuildMemberPageSource};
use crate::registration::{InteractionResponseError, Registry};

#[derive(Default)]
struct RecoveryDiscord {
    guild: Mutex<Option<policy::GuildSnapshot>>,
    sent: Mutex<Vec<(i64, String, DiscordMessage)>>,
    edits: Mutex<Vec<(i64, i64, DiscordMessage)>>,
    histories: Mutex<VecDeque<SurveyDmHistory>>,
    send_gate: Mutex<Option<Arc<Semaphore>>>,
    active_sends: AtomicUsize,
    peak_sends: AtomicUsize,
    block_edits: AtomicBool,
    edit_attempts: AtomicUsize,
    permanent_edit_error: Mutex<Option<SurveyEditErrorKind>>,
}

#[async_trait]
impl SurveyDiscordPort for RecoveryDiscord {
    async fn survey_guild_snapshot(&self, guild_id: i64) -> Result<policy::GuildSnapshot, String> {
        self.guild
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| format!("missing guild {guild_id}"))
    }

    async fn survey_role_snapshot(
        &self,
        _guild_id: i64,
        _role_id: i64,
    ) -> Result<Option<policy::RoleSnapshot>, String> {
        Ok(None)
    }

    async fn survey_send_dm(
        &self,
        discord_id: i64,
        message: DiscordMessage,
        nonce: &str,
    ) -> Result<DiscordMessageReceipt, SurveyDmError> {
        let active = self.active_sends.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_sends.fetch_max(active, Ordering::SeqCst);
        let gate = { self.send_gate.lock().unwrap().clone() };
        if let Some(gate) = gate {
            let permit = gate.acquire().await.unwrap();
            permit.forget();
        }
        self.active_sends.fetch_sub(1, Ordering::SeqCst);
        self.sent
            .lock()
            .unwrap()
            .push((discord_id, nonce.to_owned(), message));
        Ok(DiscordMessageReceipt {
            channel_id: 6_000 + u64::try_from(discord_id).unwrap(),
            message_id: 7_000 + u64::try_from(discord_id).unwrap(),
            jump_url: String::new(),
        })
    }

    async fn survey_dm_history(
        &self,
        _discord_id: i64,
        _after_unix_seconds: i64,
        _limit: usize,
    ) -> Result<SurveyDmHistory, String> {
        self.histories
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "missing history".to_owned())
    }

    async fn survey_edit_dm(
        &self,
        channel_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), SurveyEditError> {
        self.edit_attempts.fetch_add(1, Ordering::SeqCst);
        if let Some(kind) = *self.permanent_edit_error.lock().unwrap() {
            return Err(SurveyEditError {
                kind,
                message: "injected permanent edit failure".to_owned(),
            });
        }
        if self.block_edits.load(Ordering::SeqCst) {
            return Err(SurveyEditError {
                kind: SurveyEditErrorKind::Unavailable,
                message: "injected edit failure".to_owned(),
            });
        }
        self.edits
            .lock()
            .unwrap()
            .push((channel_id, message_id, message));
        Ok(())
    }
}

#[derive(Default)]
struct RecordingResponder {
    acknowledged: AtomicBool,
    fail_edits: AtomicBool,
    update_status: Mutex<VecDeque<Option<u16>>>,
    updates: Mutex<Vec<InteractionResponse>>,
    original_edits: Mutex<Vec<InteractionResponse>>,
    responses: Mutex<Vec<InteractionResponse>>,
    modals: Mutex<Vec<InteractionModal>>,
}

#[async_trait]
impl InteractionResponder for RecordingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.acknowledged.store(true, Ordering::SeqCst);
        self.responses.lock().unwrap().push(response);
        Ok(())
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.acknowledged.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.responses.lock().unwrap().push(response);
        Ok(())
    }

    async fn show_modal(&self, modal: InteractionModal) -> Result<(), InteractionResponseError> {
        self.acknowledged.store(true, Ordering::SeqCst);
        self.modals.lock().unwrap().push(modal);
        Ok(())
    }

    fn response_is_done(&self) -> bool {
        self.acknowledged.load(Ordering::SeqCst)
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.acknowledged.store(true, Ordering::SeqCst);
        self.updates.lock().unwrap().push(response);
        if let Some(status) = self.update_status.lock().unwrap().pop_front().flatten() {
            return Err(InteractionResponseError::with_status_code(
                "injected update failure",
                Some(status),
            ));
        }
        Ok(())
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        if self.fail_edits.load(Ordering::SeqCst) {
            return Err(InteractionResponseError::new("injected edit failure"));
        }
        self.original_edits.lock().unwrap().push(response.clone());
        self.responses.lock().unwrap().push(response);
        Ok(())
    }
}

struct EmptyMembers;

#[async_trait]
impl GuildMemberPageSource for EmptyMembers {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        Ok(Vec::new())
    }
}

fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    SurveyRegistrationProvider,
    Arc<RecoveryDiscord>,
    Registry,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("survey.db");
    initialize_or_migrate(&path).unwrap();
    let discord = Arc::new(RecoveryDiscord::default());
    let provider = SurveyRegistrationProvider::new(&path, discord.clone()).unwrap();
    provider.state.replace_active_guilds(&[22]);
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).unwrap();
    let registry = builder.build();
    (directory, path, provider, discord, registry)
}

fn pager_fixture(
    timeout: Duration,
) -> (
    tempfile::TempDir,
    SurveyRegistrationProvider,
    Arc<RecordingResponder>,
    SurveyPagerHandle,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("survey.db");
    initialize_or_migrate(&path).unwrap();
    let discord = Arc::new(RecoveryDiscord::default());
    let provider =
        SurveyRegistrationProvider::new_with_results_timeout(&path, discord, timeout).unwrap();
    let responder = Arc::new(RecordingResponder::default());
    let pages = (1..=3)
        .map(|index| policy::Embed {
            title: format!("Page {index}"),
            description: String::new(),
            fields: Vec::new(),
            footer: None,
        })
        .collect();
    let pager = provider
        .state
        .register_pager(9, pages, responder.clone())
        .unwrap();
    (directory, provider, responder, pager)
}

#[tokio::test]
async fn connected_guild_scope_leaves_foreign_delivery_and_edit_work_untouched() {
    let (_directory, path, provider, discord, _registry) = fixture();
    let repository = db::SurveyRepository::new(&path);
    let survey = repository
        .create_survey(99, "Foreign production survey")
        .unwrap();
    repository
        .add_question(
            99,
            survey.survey_id,
            "Recommend?",
            db::SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            99,
            survey.survey_id,
            &[44, 45],
            db::SurveyTargetType::Role,
            false,
        )
        .unwrap();
    let sent = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(sent.recipient_id, sent.attempt_count, 55, 66)
        .unwrap();

    assert_eq!(provider.state.recover(None).await.unwrap(), 0);
    assert!(discord.sent.lock().unwrap().is_empty());
    assert!(discord.edits.lock().unwrap().is_empty());
    assert_eq!(discord.edit_attempts.load(Ordering::SeqCst), 0);
    let pending = repository
        .get_response_session(survey.survey_id, 45)
        .unwrap()
        .unwrap();
    assert_eq!(
        pending.recipient.delivery_status,
        db::SurveyDeliveryStatus::Pending
    );
    let foreign_edit = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert!(!foreign_edit.recipient.controls_finalized);
}

#[tokio::test]
async fn permanent_edit_rejection_retires_terminal_controls_once() {
    let (_directory, path, provider, discord, _registry) = fixture();
    let repository = db::SurveyRepository::new(&path);
    let survey = open_survey(&repository, &[44]);
    let claimed = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(claimed.recipient_id, claimed.attempt_count, 55, 66)
        .unwrap();
    repository.close_survey(22, survey.survey_id).unwrap();
    *discord.permanent_edit_error.lock().unwrap() = Some(SurveyEditErrorKind::Forbidden);

    provider.state.recover(None).await.unwrap();
    provider.state.recover(None).await.unwrap();

    assert_eq!(discord.edit_attempts.load(Ordering::SeqCst), 1);
    let retired = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert!(retired.recipient.controls_finalized);
}

#[tokio::test]
async fn transient_edit_rejection_uses_backoff_instead_of_tight_retrying() {
    let (_directory, path, provider, discord, _registry) = fixture();
    let repository = db::SurveyRepository::new(&path);
    let survey = open_survey(&repository, &[44]);
    let claimed = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(claimed.recipient_id, claimed.attempt_count, 55, 66)
        .unwrap();
    discord.block_edits.store(true, Ordering::SeqCst);

    provider.state.recover(None).await.unwrap();
    provider.state.recover(None).await.unwrap();

    assert_eq!(discord.edit_attempts.load(Ordering::SeqCst), 1);
    let pending = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert!(!pending.recipient.controls_finalized);
}

async fn page_click(
    provider: &SurveyRegistrationProvider,
    custom_id: String,
    user_id: u64,
    responder: Arc<RecordingResponder>,
) {
    provider
        .handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 1,
                custom_id,
                user_id,
                user_display_name: "Admin".to_owned(),
                guild_id: Some(22),
                channel_id: Some(33),
                member_permissions: Some(ADMINISTRATOR_PERMISSION),
                values: Vec::new(),
            },
            responder,
        )
        .await
        .unwrap();
}

async fn pager_controls(pager: &SurveyPagerHandle) -> SurveyPagerControls {
    pager.lock().await.controls.clone()
}

#[tokio::test(start_paused = true)]
async fn pager_success_refreshes_the_sliding_timeout_and_stale_timer_cannot_retire_it() {
    let (_directory, provider, initial, pager) =
        pager_fixture(Duration::from_secs(policy::RESULTS_TIMEOUT_SECONDS as u64));
    let controls = pager_controls(&pager).await;
    provider.state.arm_initial_pager_timeout(&pager).await;
    assert_eq!(pager.lock().await.timeout_generation, 1);
    tokio::task::yield_now().await;

    tokio::time::advance(Duration::from_secs(599)).await;
    let fresh = Arc::new(RecordingResponder::default());
    page_click(&provider, controls.next_custom_id.clone(), 9, fresh.clone()).await;
    assert_eq!(pager.lock().await.timeout_generation, 2);
    tokio::task::yield_now().await;
    assert_eq!(
        fresh.updates.lock().unwrap()[0].embeds[0].title.as_deref(),
        Some("Page 2")
    );

    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::task::yield_now().await;
    assert!(initial.original_edits.lock().unwrap().is_empty());
    assert!(fresh.original_edits.lock().unwrap().is_empty());
    assert!(provider.state.pager(&controls.next_custom_id).is_some());

    tokio::time::advance(Duration::from_secs(598)).await;
    tokio::task::yield_now().await;
    assert!(initial.original_edits.lock().unwrap().is_empty());
    let retired = fresh.original_edits.lock().unwrap();
    assert_eq!(retired.len(), 1);
    assert!(
        retired[0].components[0]
            .buttons
            .iter()
            .all(|button| button.disabled)
    );
    assert!(provider.state.pager(&controls.next_custom_id).is_none());
}

#[tokio::test]
async fn unknown_pager_after_restart_is_acknowledged_and_disabled() {
    let (directory, provider, initial, pager) = pager_fixture(Duration::from_secs(600));
    let controls = pager_controls(&pager).await;
    provider.state.discard_pager(&pager).await;
    drop(provider);
    drop(initial);

    let path = directory.path().join("survey.db");
    let restarted = SurveyRegistrationProvider::new_with_results_timeout(
        &path,
        Arc::new(RecoveryDiscord::default()),
        Duration::from_secs(600),
    )
    .unwrap();
    let responder = Arc::new(RecordingResponder::default());
    page_click(&restarted, controls.next_custom_id, 9, responder.clone()).await;

    let updates = responder.updates.lock().unwrap();
    assert_eq!(updates.len(), 1);
    assert!(updates[0].content.is_empty());
    assert!(updates[0].embeds.is_empty());
    assert!(
        updates[0].components[0]
            .buttons
            .iter()
            .all(|button| button.disabled)
    );
}

#[tokio::test]
async fn recreated_provider_ready_recovers_once_and_rejects_stale_routes_without_mutation() {
    let (directory, path, provider, discord, registry) = fixture();
    let repository = db::SurveyRepository::new(&path);
    let survey = open_survey(&repository, &[44]);
    let question = repository.get_questions(22, survey.survey_id).unwrap()[0].clone();

    // Exercise the production delivery path before the process boundary.  It
    // commits the stable nonce and real DM identity to the current schema.
    assert_eq!(provider.state.recover(None).await.unwrap(), 1);
    assert_eq!(discord.sent.lock().unwrap().len(), 1);
    assert_eq!(discord.sent.lock().unwrap()[0].1, policy::delivery_nonce(1));
    let delivery_edit_count = discord.edits.lock().unwrap().len();
    assert_eq!(delivery_edit_count, 1);

    // Model a response mutation that committed immediately before restart,
    // while the terminal Discord edit/finalization did not. READY must recover
    // this durable boundary exactly once.
    repository
        .save_answer(
            survey.survey_id,
            44,
            question.question_id,
            db::SurveyAnswer::Numeric(10),
        )
        .unwrap();
    assert!(repository.submit_response(survey.survey_id, 44).unwrap());
    let committed = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert!(committed.recipient.submitted_at.is_some());
    assert!(!committed.recipient.controls_finalized);

    // Retain IDs from an ephemeral pager that existed before restart. The new
    // provider must not accidentally revive this in-memory route.
    let old_pager_responder = Arc::new(RecordingResponder::default());
    let old_pager = provider
        .state
        .register_pager(
            9,
            vec![
                policy::Embed {
                    title: "Page 1".to_owned(),
                    description: String::new(),
                    fields: Vec::new(),
                    footer: None,
                },
                policy::Embed {
                    title: "Page 2".to_owned(),
                    description: String::new(),
                    fields: Vec::new(),
                    footer: None,
                },
            ],
            old_pager_responder,
        )
        .unwrap();
    provider.state.arm_initial_pager_timeout(&old_pager).await;
    let old_controls = pager_controls(&old_pager).await;

    drop(old_pager);
    drop(registry);
    drop(provider);

    let restarted = SurveyRegistrationProvider::new_with_results_timeout(
        &path,
        discord.clone(),
        Duration::from_secs(600),
    )
    .unwrap();
    let mut builder = RegistryBuilder::default();
    restarted.register(&mut builder).unwrap();
    let restarted_registry = builder.build();

    let first_ready = restarted
        .gateway_observer()
        .ready_recovery(ready_context())
        .await;
    assert!(first_ready.failures.is_empty());
    assert_eq!(first_ready.guilds_refreshed, 1);
    assert_eq!(discord.sent.lock().unwrap().len(), 1);
    assert_eq!(
        discord.edits.lock().unwrap().len(),
        delivery_edit_count + 1,
        "first READY performs the one deferred terminal edit"
    );
    let recovered = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert!(recovered.recipient.controls_finalized);

    let second_ready = restarted
        .gateway_observer()
        .ready_recovery(ready_context())
        .await;
    assert!(second_ready.failures.is_empty());
    assert_eq!(second_ready.guilds_refreshed, 1);
    assert_eq!(discord.sent.lock().unwrap().len(), 1);
    assert_eq!(
        discord.edits.lock().unwrap().len(),
        delivery_edit_count + 1,
        "repeat READY neither resends nor re-edits finalized controls"
    );
    let stable = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert_eq!(stable, recovered);

    // A pre-restart pager ID is acknowledged with both controls disabled. It
    // cannot page a newly-created report or touch the durable response.
    let stale_pager = Arc::new(RecordingResponder::default());
    restarted_registry
        .component_handler(&old_controls.next_custom_id)
        .unwrap()
        .handle(
            InteractionRequest::Component {
                interaction_id: 20,
                custom_id: old_controls.next_custom_id,
                user_id: 9,
                user_display_name: "Admin".to_owned(),
                guild_id: Some(22),
                channel_id: Some(33),
                member_permissions: Some(ADMINISTRATOR_PERMISSION),
                values: Vec::new(),
            },
            stale_pager.clone(),
        )
        .await
        .unwrap();
    {
        let pager_updates = stale_pager.updates.lock().unwrap();
        assert_eq!(pager_updates.len(), 1);
        assert!(
            pager_updates[0].components[0]
                .buttons
                .iter()
                .all(|button| button.disabled)
        );
    }

    // A stale question control from the now-submitted response receives one
    // private acknowledgement. The repository rejects it before an answer,
    // response edit, DM send, or recovery edit can be duplicated.
    let stale_response = Arc::new(RecordingResponder::default());
    restarted_registry
        .component_handler(&format!(
            "survey:{}:question:{}:score",
            survey.survey_id, question.question_id
        ))
        .unwrap()
        .handle(
            InteractionRequest::Component {
                interaction_id: 21,
                custom_id: format!(
                    "survey:{}:question:{}:score",
                    survey.survey_id, question.question_id
                ),
                user_id: 44,
                user_display_name: "Respondent".to_owned(),
                guild_id: None,
                channel_id: Some(6_044),
                member_permissions: None,
                values: vec!["1".to_owned()],
            },
            stale_response.clone(),
        )
        .await
        .unwrap();
    {
        let stale_messages = stale_response.responses.lock().unwrap();
        assert_eq!(stale_messages.len(), 1);
        assert!(stale_messages[0].ephemeral);
        assert!(stale_messages[0].content.contains("already been submitted"));
    }
    assert!(stale_response.original_edits.lock().unwrap().is_empty());
    assert_eq!(discord.sent.lock().unwrap().len(), 1);
    assert_eq!(discord.edits.lock().unwrap().len(), delivery_edit_count + 1);
    assert_eq!(
        repository
            .get_response_session(survey.survey_id, 44)
            .unwrap()
            .unwrap(),
        stable
    );

    drop(restarted_registry);
    drop(restarted);
    drop(directory);
}

#[tokio::test(start_paused = true)]
async fn pager_not_found_is_terminal_and_retires_through_the_previous_token() {
    let (_directory, provider, initial, pager) = pager_fixture(Duration::from_secs(600));
    let controls = pager_controls(&pager).await;
    provider.state.arm_initial_pager_timeout(&pager).await;
    let dead = Arc::new(RecordingResponder::default());
    dead.update_status.lock().unwrap().push_back(Some(404));

    page_click(&provider, controls.next_custom_id.clone(), 9, dead.clone()).await;
    assert!(pager.lock().await.retired);
    assert!(provider.state.pager(&controls.next_custom_id).is_none());
    {
        let retired = initial.original_edits.lock().unwrap();
        assert_eq!(retired.len(), 1);
        assert!(
            retired[0].components[0]
                .buttons
                .iter()
                .all(|button| button.disabled)
        );
    }

    tokio::time::advance(Duration::from_secs(600)).await;
    tokio::task::yield_now().await;
    assert_eq!(initial.original_edits.lock().unwrap().len(), 1);
    assert!(dead.original_edits.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn pager_transient_failure_stays_live_and_a_retry_owns_timeout_cleanup() {
    let (_directory, provider, initial, pager) = pager_fixture(Duration::from_secs(600));
    let controls = pager_controls(&pager).await;
    provider.state.arm_initial_pager_timeout(&pager).await;
    let transient = Arc::new(RecordingResponder::default());
    transient.update_status.lock().unwrap().push_back(Some(500));
    page_click(
        &provider,
        controls.next_custom_id.clone(),
        9,
        transient.clone(),
    )
    .await;
    assert!(!pager.lock().await.retired);
    assert!(provider.state.pager(&controls.next_custom_id).is_some());
    assert!(transient.responses.lock().unwrap().is_empty());

    let retry = Arc::new(RecordingResponder::default());
    page_click(&provider, controls.next_custom_id.clone(), 9, retry.clone()).await;
    assert_eq!(pager.lock().await.timeout_generation, 3);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(600)).await;
    tokio::task::yield_now().await;

    assert!(initial.original_edits.lock().unwrap().is_empty());
    assert!(transient.original_edits.lock().unwrap().is_empty());
    assert_eq!(retry.original_edits.lock().unwrap().len(), 1);
    assert!(provider.state.pager(&controls.next_custom_id).is_none());
}

fn open_survey(repository: &db::SurveyRepository, recipients: &[i64]) -> db::Survey {
    let survey = repository.create_survey(22, "Recovery").unwrap();
    repository
        .add_question(
            22,
            survey.survey_id,
            "Recommend?",
            db::SurveyQuestionType::Nps,
            true,
        )
        .unwrap();
    repository
        .open_survey(
            22,
            survey.survey_id,
            recipients,
            if recipients.len() == 1 {
                db::SurveyTargetType::Member
            } else {
                db::SurveyTargetType::Role
            },
            false,
        )
        .unwrap();
    survey
}

fn ready_context() -> ReadyRecoveryContext {
    ReadyRecoveryContext::new(Arc::<[u64]>::from([22]), Arc::new(EmptyMembers))
}

#[tokio::test]
async fn ambiguous_history_receipt_is_recovered_without_resending() {
    let (directory, path, provider, discord, _registry) = fixture();
    let repository = db::SurveyRepository::new(&path);
    let survey = open_survey(&repository, &[44]);
    let question = repository.get_questions(22, survey.survey_id).unwrap()[0].clone();
    let claim = repository.claim_deliveries(1, 300).unwrap().remove(0);
    rusqlite::Connection::open(directory.path().join("survey.db"))
        .unwrap()
        .execute(
            "UPDATE survey_recipients SET claimed_at=1,updated_at=1 WHERE recipient_id=?1",
            [claim.recipient_id],
        )
        .unwrap();
    discord
        .histories
        .lock()
        .unwrap()
        .push_back(SurveyDmHistory {
            channel_id: 6_044,
            bot_user_id: 999,
            messages: vec![policy::HistoryMessage {
                message_id: 7_044,
                author_id: 999,
                nonce: None,
                components: vec![policy::MessageComponent {
                    custom_id: Some(format!(
                        "survey:{}:question:{}:score",
                        survey.survey_id, question.question_id
                    )),
                }],
            }],
        });

    let report = provider
        .gateway_observer()
        .ready_recovery(ready_context())
        .await;
    assert!(report.failures.is_empty());
    assert!(discord.sent.lock().unwrap().is_empty());
    let session = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert_eq!(
        session.recipient.delivery_status,
        db::SurveyDeliveryStatus::Sent
    );
    assert_eq!(session.recipient.dm_message_id, Some(7_044));
}

#[tokio::test]
async fn supervised_worker_delivery_is_bounded_to_python_limit_of_five() {
    let (_directory, path, provider, discord, _registry) = fixture();
    let repository = db::SurveyRepository::new(&path);
    open_survey(&repository, &(40..48).collect::<Vec<_>>());
    let gate = Arc::new(Semaphore::new(0));
    *discord.send_gate.lock().unwrap() = Some(Arc::clone(&gate));
    let spec = provider.recovery_worker_spec();
    assert_eq!(spec.name, SURVEY_RECOVERY_WORKER_NAME);
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let worker = Arc::clone(&spec.worker);
    let task = tokio::spawn(async move { worker.run(WorkerContext::new(receiver)).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while discord.peak_sends.load(Ordering::SeqCst) < policy::DELIVERY_CONCURRENCY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(discord.active_sends.load(Ordering::SeqCst), 5);
    assert_eq!(discord.peak_sends.load(Ordering::SeqCst), 5);
    gate.add_permits(8);
    while discord.sent.lock().unwrap().len() < 8 {
        tokio::task::yield_now().await;
    }
    shutdown.send(true).unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(discord.peak_sends.load(Ordering::SeqCst), 5);
}

fn command(user_id: u64, permissions: u64, option: InteractionOption) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "survey".to_owned(),
        user_id,
        user_display_name: "Admin".to_owned(),
        guild_id: Some(22),
        channel_id: Some(33),
        member_permissions: Some(permissions),
        options: vec![option],
    }
}

fn subcommand(name: &str, options: Vec<InteractionOption>) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::Subcommand(options),
    }
}

fn value(name: &str, value: InteractionValue) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value,
    }
}

#[tokio::test]
async fn migrated_interaction_campaign_runs_create_dm_select_text_and_ready_finalization() {
    let (_directory, path, provider, discord, registry) = fixture();
    *discord.guild.lock().unwrap() = Some(policy::GuildSnapshot {
        guild_id: 22,
        members: vec![policy::MemberSnapshot {
            discord_id: 44,
            guild_id: 22,
            bot: false,
        }],
    });
    let handler = registry.command_handler("survey").unwrap();
    let admin = ADMINISTRATOR_PERMISSION;

    handler
        .handle(
            command(
                9,
                admin,
                subcommand(
                    "create",
                    vec![value("title", InteractionValue::String("E2E".to_owned()))],
                ),
            ),
            Arc::new(RecordingResponder::default()),
        )
        .await
        .unwrap();
    let repository = db::SurveyRepository::new(&path);
    let survey = repository.list_surveys(22, None).unwrap().remove(0);

    for (question_type, prompt) in [("nps", "Recommend?"), ("text", "Why?")] {
        handler
            .handle(
                command(
                    9,
                    admin,
                    InteractionOption {
                        name: "question".to_owned(),
                        value: InteractionValue::SubcommandGroup(vec![subcommand(
                            "add",
                            vec![
                                value("survey_id", InteractionValue::Integer(survey.survey_id)),
                                value(
                                    "question_type",
                                    InteractionValue::String(question_type.to_owned()),
                                ),
                                value("prompt", InteractionValue::String(prompt.to_owned())),
                                value("required", InteractionValue::Boolean(true)),
                            ],
                        )]),
                    },
                ),
                Arc::new(RecordingResponder::default()),
            )
            .await
            .unwrap();
    }
    let questions = repository.get_questions(22, survey.survey_id).unwrap();

    handler
        .handle(
            command(
                9,
                admin,
                subcommand(
                    "send",
                    vec![
                        value("survey_id", InteractionValue::Integer(survey.survey_id)),
                        value("target", InteractionValue::String("member".to_owned())),
                        value(
                            "member",
                            InteractionValue::User {
                                id: 44,
                                display_name: Some("Respondent".to_owned()),
                                is_bot: Some(false),
                            },
                        ),
                    ],
                ),
            ),
            Arc::new(RecordingResponder::default()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while discord.sent.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    {
        let sent = discord.sent.lock().unwrap();
        assert_eq!(sent[0].1, policy::delivery_nonce(1));
        assert!(sent[0].2.response.components[0].string_select.is_some());
    }

    let component = registry
        .component_handler(&format!(
            "survey:{}:question:{}:score",
            survey.survey_id, questions[0].question_id
        ))
        .unwrap();
    let score_response = Arc::new(RecordingResponder::default());
    component
        .handle(
            InteractionRequest::Component {
                interaction_id: 2,
                custom_id: format!(
                    "survey:{}:question:{}:score",
                    survey.survey_id, questions[0].question_id
                ),
                user_id: 44,
                user_display_name: "Respondent".to_owned(),
                guild_id: None,
                channel_id: Some(6_044),
                member_permissions: None,
                values: vec!["10".to_owned()],
            },
            score_response.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        score_response.responses.lock().unwrap()[0].components[0].buttons[0].label,
        "Write answer"
    );

    let text_id = format!(
        "survey:{}:question:{}:text",
        survey.survey_id, questions[1].question_id
    );
    let text_response = Arc::new(RecordingResponder::default());
    component
        .handle(
            InteractionRequest::Component {
                interaction_id: 3,
                custom_id: text_id,
                user_id: 44,
                user_display_name: "Respondent".to_owned(),
                guild_id: None,
                channel_id: Some(6_044),
                member_permissions: None,
                values: Vec::new(),
            },
            text_response.clone(),
        )
        .await
        .unwrap();
    let modal = text_response.modals.lock().unwrap()[0].clone();
    assert_eq!(modal.inputs[0].max_length, Some(1_000));
    let modal_response = Arc::new(RecordingResponder::default());
    component
        .handle(
            InteractionRequest::Modal {
                interaction_id: 4,
                custom_id: modal.custom_id,
                user_id: 44,
                guild_id: None,
                channel_id: Some(6_044),
                member_permissions: None,
                fields: BTreeMap::from([(TEXT_MODAL_FIELD.to_owned(), "Great".to_owned())]),
            },
            modal_response.clone(),
        )
        .await
        .unwrap();
    assert!(
        modal_response.responses.lock().unwrap()[0].components[0]
            .string_select
            .is_some()
    );

    discord.block_edits.store(true, Ordering::SeqCst);
    let submit_response = Arc::new(RecordingResponder::default());
    submit_response.fail_edits.store(true, Ordering::SeqCst);
    component
        .handle(
            InteractionRequest::Component {
                interaction_id: 5,
                custom_id: format!("survey:{}:review:submit", survey.survey_id),
                user_id: 44,
                user_display_name: "Respondent".to_owned(),
                guild_id: None,
                channel_id: Some(6_044),
                member_permissions: None,
                values: Vec::new(),
            },
            submit_response,
        )
        .await
        .unwrap();
    let submitted = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert!(submitted.recipient.submitted_at.is_some());
    assert!(!submitted.recipient.controls_finalized);

    discord.block_edits.store(false, Ordering::SeqCst);
    let report = provider
        .gateway_observer()
        .ready_recovery(ready_context())
        .await;
    assert!(report.failures.is_empty());
    let finalized = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert!(finalized.recipient.controls_finalized);
    assert_eq!(discord.edits.lock().unwrap().last().unwrap().1, 7_044);
}

#[tokio::test]
async fn admin_guard_and_component_owner_errors_are_private_and_do_not_mutate() {
    let (_directory, path, _provider, _discord, registry) = fixture();
    let handler = registry.command_handler("survey").unwrap();
    let denied = Arc::new(RecordingResponder::default());
    handler
        .handle(
            command(
                9,
                0,
                subcommand(
                    "create",
                    vec![value("title", InteractionValue::String("Nope".to_owned()))],
                ),
            ),
            denied.clone(),
        )
        .await
        .unwrap();
    {
        let responses = denied.responses.lock().unwrap();
        let denied_message = &responses[0];
        assert!(denied_message.ephemeral);
        assert_eq!(
            denied_message.content,
            "❌ Admin only. You need Administrator or Manage Server permissions."
        );
    }
    assert!(
        db::SurveyRepository::new(&path)
            .list_surveys(22, None)
            .unwrap()
            .is_empty()
    );

    let wrong_owner = Arc::new(RecordingResponder::default());
    registry
        .component_handler("survey:999:review:submit")
        .unwrap()
        .handle(
            InteractionRequest::Component {
                interaction_id: 6,
                custom_id: "survey:999:review:submit".to_owned(),
                user_id: 45,
                user_display_name: "Wrong".to_owned(),
                guild_id: None,
                channel_id: Some(6_044),
                member_permissions: None,
                values: Vec::new(),
            },
            wrong_owner.clone(),
        )
        .await
        .unwrap();
    let owner_message = &wrong_owner.responses.lock().unwrap()[0];
    assert!(owner_message.ephemeral);
    assert_eq!(
        owner_message.content,
        "❌ This survey response isn't yours."
    );
}
