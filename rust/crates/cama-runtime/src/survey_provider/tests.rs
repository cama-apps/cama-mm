use std::collections::VecDeque;
use std::sync::Mutex;

use cama_db::schema_manager::initialize_or_migrate;

use super::*;

#[derive(Default)]
struct RecordingDiscord {
    guild: Mutex<Option<policy::GuildSnapshot>>,
    role: Mutex<Option<policy::RoleSnapshot>>,
    sent: Mutex<Vec<(i64, String, DiscordMessage)>>,
    edits: Mutex<Vec<(i64, i64, DiscordMessage)>>,
    histories: Mutex<VecDeque<SurveyDmHistory>>,
}

#[async_trait]
impl SurveyDiscordPort for RecordingDiscord {
    async fn survey_guild_snapshot(&self, _guild_id: i64) -> Result<policy::GuildSnapshot, String> {
        self.guild
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "missing guild".to_owned())
    }

    async fn survey_role_snapshot(
        &self,
        _guild_id: i64,
        _role_id: i64,
    ) -> Result<Option<policy::RoleSnapshot>, String> {
        Ok(self.role.lock().unwrap().clone())
    }

    async fn survey_send_dm(
        &self,
        discord_id: i64,
        message: DiscordMessage,
        nonce: &str,
    ) -> Result<DiscordMessageReceipt, SurveyDmError> {
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
    ) -> Result<(), String> {
        self.edits
            .lock()
            .unwrap()
            .push((channel_id, message_id, message));
        Ok(())
    }
}

fn fixture() -> (
    tempfile::TempDir,
    SurveyRegistrationProvider,
    Arc<RecordingDiscord>,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("survey.db");
    initialize_or_migrate(&path).unwrap();
    let discord = Arc::new(RecordingDiscord::default());
    let provider = SurveyRegistrationProvider::new(&path, discord.clone()).unwrap();
    (directory, provider, discord)
}

#[test]
fn production_registration_contains_all_twelve_python_leaf_commands_and_persistent_route() {
    let (_directory, provider, _discord) = fixture();
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).unwrap();
    let registry = builder.build();
    let command = registry.commands().next().unwrap();
    assert_eq!(command.name, "survey");
    assert_eq!(command.options.len(), 9);
    let question = command
        .options
        .iter()
        .find(|option| option.name == "question")
        .unwrap();
    assert_eq!(question.options.len(), 4);
    assert_eq!(registry.component_routes().len(), 1);
    assert_eq!(registry.component_routes()[0].custom_id_prefix, "survey:");

    let send = command
        .options
        .iter()
        .find(|option| option.name == "send")
        .unwrap();
    assert_eq!(send.options.len(), 5);
    let target = send
        .options
        .iter()
        .find(|option| option.name == "target")
        .unwrap();
    assert_eq!(target.choices.len(), 3);
}

#[test]
fn rendered_numeric_and_review_controls_use_real_string_select_rows() {
    let session = db::SurveySession {
        survey: db::Survey {
            survey_id: 11,
            guild_id: 22,
            title: "Private survey".to_owned(),
            status: db::SurveyStatus::Open,
            target_type: Some(db::SurveyTargetType::Role),
            registered_only: false,
            created_at: 1,
            opened_at: Some(2),
            closed_at: None,
        },
        recipient: db::SurveyRecipient {
            recipient_id: 33,
            survey_id: 11,
            guild_id: 22,
            discord_id: 44,
            delivery_status: db::SurveyDeliveryStatus::Sent,
            attempt_count: 1,
            receipt_checked_attempt: 1,
            retry_requested: false,
            claimed_at: None,
            dm_channel_id: Some(55),
            dm_message_id: Some(66),
            ui_channel_id: Some(55),
            ui_message_id: Some(66),
            controls_finalized: false,
            current_question_id: Some(77),
            in_review: false,
            started_at: Some(3),
            submitted_at: None,
            last_error: None,
            created_at: 2,
            updated_at: 3,
        },
        questions: vec![db::SurveyQuestion {
            question_id: 77,
            survey_id: 11,
            guild_id: 22,
            position: 1,
            prompt: "Recommend?".to_owned(),
            question_type: db::SurveyQuestionType::Nps,
            required: false,
            created_at: 1,
        }],
        answers: Vec::new(),
    };
    let message = render_message(&session).unwrap();
    assert_eq!(message.response.components.len(), 2);
    let select = message.response.components[0]
        .string_select
        .as_ref()
        .unwrap();
    assert_eq!(select.custom_id, "survey:11:question:77:score");
    assert_eq!(select.options.len(), 11);
    assert_eq!(message.response.components[1].buttons[0].label, "Skip");
}

#[tokio::test]
async fn scheduled_recovery_delivers_with_stable_nonce_and_persists_receipt() {
    let (_directory, provider, discord) = fixture();
    let repository = provider.state.repository.clone();
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
            &[44],
            db::SurveyTargetType::Member,
            false,
        )
        .unwrap();

    assert_eq!(provider.state.recover(None).await.unwrap(), 1);
    let sent = discord.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, 44);
    assert_eq!(sent[0].1, policy::delivery_nonce(1));
    assert!(
        sent[0].2.response.allowed_mentions
            == crate::registration::InteractionAllowedMentions::None
    );
    drop(sent);
    let session = repository
        .get_response_session(survey.survey_id, 44)
        .unwrap()
        .unwrap();
    assert_eq!(
        session.recipient.delivery_status,
        db::SurveyDeliveryStatus::Sent
    );
    assert_eq!(session.recipient.dm_channel_id, Some(6_044));
    assert_eq!(session.recipient.dm_message_id, Some(7_044));
}

#[tokio::test]
async fn ready_recovery_is_repeatable_and_skips_identical_response_edits() {
    let (_directory, provider, discord) = fixture();
    let repository = provider.state.repository.clone();
    let survey = repository.create_survey(22, "Repeatable").unwrap();
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
            &[44],
            db::SurveyTargetType::Member,
            false,
        )
        .unwrap();
    let claimed = repository.claim_deliveries(1, 300).unwrap().remove(0);
    repository
        .mark_delivery_sent(claimed.recipient_id, claimed.attempt_count, 55, 66)
        .unwrap();

    provider.state.reconcile_response_messages().await.unwrap();
    provider.state.reconcile_response_messages().await.unwrap();
    assert_eq!(discord.edits.lock().unwrap().len(), 1);
}

#[test]
fn response_component_parser_rejects_cross_shape_ids_and_accepts_modal_suffix() {
    assert_eq!(
        parse_response_component("survey:11:question:77:text-modal")
            .unwrap()
            .kind,
        ResponseActionKind::Text
    );
    assert!(parse_response_component("survey:11:question:nope:score").is_err());
    assert!(parse_response_component("survey:11:review:unknown").is_err());
}
