use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{Days, Utc};
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::registration::{InteractionMessageDelivery, Registry, RegistryBuilder};
use crate::test_support::initialize_test_database as initialize_or_migrate;

const GUILD: i64 = 9_001;
const CHANNEL: i64 = 9_002;
const USER: i64 = 101;

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn migrated() -> Self {
        let database = NamedTempFile::new().expect("temporary player-trivia database");
        initialize_or_migrate(database.path()).expect("migrate player-trivia fixture");
        let fixture = Self { database };
        for offset in 0..4 {
            fixture
                .connection()
                .execute(
                    "INSERT INTO players (
                     guild_id, discord_id, discord_username, wins, losses,
                     jopacoin_balance, glicko_rating, glicko_rd, os_mu, os_sigma
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 3, ?6, 80, ?7, 3)",
                    params![
                        GUILD,
                        USER + offset,
                        format!("Player {}", offset + 1),
                        90 - offset * 10,
                        10 + offset * 10,
                        1_900.0 - offset as f64 * 100.0,
                        45.0 - offset as f64 * 2.0,
                    ],
                )
                .expect("insert player-trivia player");
        }
        fixture
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("open fixture database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production foreign-key policy");
        connection
    }

    fn repository(&self) -> db::PlayerTriviaRepository {
        db::PlayerTriviaRepository::new(self.database.path())
    }

    fn balance(&self, user_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, user_id],
                |row| row.get(0),
            )
            .expect("read player balance")
    }

    fn seed_event(&self, effects: &str) {
        let today = Utc::now().date_naive();
        let dates = [
            today,
            today.checked_sub_days(Days::new(1)).expect("yesterday"),
        ];
        let now = unix_timestamp().expect("timestamp");
        for date in dates {
            self.connection()
                .execute(
                    "INSERT INTO economy_daily_events (
                     guild_id,event_date,name,hero,direction,severity,target_effect_jc,
                     forecast_flow_jc,expected_effect_jc,monetary_stock_before,effects,
                     announcement,starts_at,ends_at,created_at
                 ) VALUES (?1,?2,'Trivia Test','Invoker','boon',1,0,0,0,0,?3,
                           'Trivia event',?4,?5,?4)",
                    params![GUILD, date.to_string(), effects, now - 60, now + 3_600],
                )
                .expect("seed player-trivia economy event");
        }
    }
}

#[derive(Default)]
struct FakeDiscord {
    gamba: bool,
    members: BTreeSet<i64>,
    edits: Mutex<Vec<(InteractionMessageReceipt, InteractionResponse)>>,
}

#[async_trait]
impl PlayerTriviaDiscordPort for FakeDiscord {
    async fn player_trivia_channel_is_gamba(
        &self,
        _guild_id: i64,
        _channel_id: i64,
    ) -> Result<bool, String> {
        Ok(self.gamba)
    }

    fn player_trivia_non_bot_member_ids(
        &self,
        _guild_id: i64,
    ) -> Result<Option<BTreeSet<i64>>, String> {
        Ok(Some(self.members.clone()))
    }

    async fn player_trivia_user_avatar_url(
        &self,
        _user_id: i64,
        _guild_id: i64,
    ) -> Result<Option<String>, String> {
        Ok(Some("https://cdn.test/player-trivia.png".to_owned()))
    }

    async fn edit_player_trivia_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.edits.lock().expect("edits").push((receipt, response));
        Ok(())
    }
}

#[derive(Default)]
struct Captured {
    immediate: Vec<InteractionResponse>,
    defers: Vec<bool>,
    followups: Vec<InteractionResponse>,
    updates: Vec<InteractionResponse>,
}

struct RecordingResponder {
    captured: Mutex<Captured>,
    receipt: Option<InteractionMessageReceipt>,
    fail_followup: bool,
}

impl Default for RecordingResponder {
    fn default() -> Self {
        Self {
            captured: Mutex::new(Captured::default()),
            receipt: Some(InteractionMessageReceipt {
                message_id: 44,
                channel_id: CHANNEL as u64,
                delivery: InteractionMessageDelivery::InteractionFollowup,
            }),
            fail_followup: false,
        }
    }
}

#[async_trait]
impl InteractionResponder for RecordingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("captured")
            .immediate
            .push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("captured")
            .defers
            .push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        if self.fail_followup {
            return Err(InteractionResponseError::new("send failed"));
        }
        self.captured
            .lock()
            .expect("captured")
            .followups
            .push(response);
        Ok(())
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.followup(response).await?;
        Ok(self.receipt)
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("captured")
            .updates
            .push(response);
        Ok(())
    }
}

fn config(path: &Path, overrides: &[(&str, &str)]) -> ApplicationConfig {
    let mut values = BTreeMap::from([
        ("DISCORD_BOT_TOKEN".to_owned(), "test-token".to_owned()),
        ("DB_PATH".to_owned(), path.display().to_string()),
        ("PLAYER_TRIVIA_QUESTION_COUNT".to_owned(), "1".to_owned()),
        (
            "PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS".to_owned(),
            "3600".to_owned(),
        ),
        (
            "PLAYER_TRIVIA_COOLDOWN_SECONDS".to_owned(),
            "86400".to_owned(),
        ),
        ("PLAYER_TRIVIA_INCLUDE_SPICY".to_owned(), "false".to_owned()),
        (
            "PLAYER_TRIVIA_REWARD_PER_CORRECT".to_owned(),
            "1".to_owned(),
        ),
        ("ECONOMY_EVENTS_ENABLED".to_owned(), "false".to_owned()),
    ]);
    values.extend(
        overrides
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    ApplicationConfig::from_lookup(|name| values.get(name).cloned()).expect("test configuration")
}

fn discord() -> Arc<FakeDiscord> {
    discord_with_gamba(true)
}

fn discord_with_gamba(gamba: bool) -> Arc<FakeDiscord> {
    Arc::new(FakeDiscord {
        gamba,
        members: (USER..USER + 4).collect(),
        edits: Mutex::new(Vec::new()),
    })
}

fn provider(
    fixture: &Fixture,
    config: &ApplicationConfig,
    discord: Arc<FakeDiscord>,
) -> PlayerTriviaRegistrationProvider {
    PlayerTriviaRegistrationProvider::new(fixture.database.path(), config, discord)
}

fn registry(provider: &PlayerTriviaRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(provider)
        .expect("register player-trivia provider");
    builder.build()
}

fn command(user_id: i64, permissions: Option<u64>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: user_id as u64 + 10_000,
        name: "playertrivia".to_owned(),
        user_id: user_id as u64,
        user_display_name: "Trivia Player".to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(CHANNEL as u64),
        member_permissions: permissions,
        options: Vec::new(),
    }
}

fn dm_command(user_id: i64) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: user_id as u64 + 10_000,
        name: "playertrivia".to_owned(),
        user_id: user_id as u64,
        user_display_name: "Trivia Player".to_owned(),
        guild_id: None,
        channel_id: Some(CHANNEL as u64),
        member_permissions: None,
        options: Vec::new(),
    }
}

fn component(custom_id: String, user_id: i64) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: user_id as u64 + 20_000,
        custom_id,
        user_id: user_id as u64,
        user_display_name: "Trivia Player".to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(CHANNEL as u64),
        member_permissions: None,
        values: Vec::new(),
    }
}

fn questions(count: usize) -> Vec<db::PlayerTriviaQuestionRecord> {
    (0..count)
        .map(|index| db::PlayerTriviaQuestionRecord {
            question_key: format!("matches:wins:{index}"),
            category: "matches".to_owned(),
            question_text: format!("Who won match question {}?", index + 1),
            options: ["Alpha", "Bravo", "Charlie", "Delta"].map(str::to_owned),
            correct_index: 1,
            explanation: "Computed from recorded inhouse matches.".to_owned(),
            spicy: false,
        })
        .collect()
}

fn start_manual(fixture: &Fixture, count: usize) -> i64 {
    fixture
        .repository()
        .try_start_session(
            USER,
            GUILD,
            &questions(count),
            unix_timestamp().expect("now"),
            0,
            true,
        )
        .expect("start manual session")
        .expect("claimed")
}

fn latest_session(fixture: &Fixture, user_id: i64) -> db::StoredPlayerTriviaSession {
    fixture
        .repository()
        .active_session_for_player(user_id, GUILD)
        .expect("active session lookup")
        .expect("active session")
}

fn question() -> app::PlayerTriviaQuestion {
    app::PlayerTriviaQuestion::new(
        "matches:wins:1",
        "matches",
        "Who won the most?",
        ["Alpha", "Bravo", "Charlie", "Delta"].map(str::to_owned),
        1,
        "Computed from recorded inhouse matches.",
        false,
    )
    .expect("valid question")
}

#[test]
fn component_ids_are_strict_and_round_trip() {
    assert_eq!(
        parse_component("player_trivia:42:3:1").expect("parse"),
        ParsedComponent {
            session_id: 42,
            question_number: 3,
            selected_index: 1
        }
    );
    for invalid in [
        "player_trivia:42:3",
        "player_trivia:42:3:4",
        "player_trivia:0:3:1",
        "trivia:42:3:1",
    ] {
        assert!(parse_component(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn every_player_trivia_policy_knob_comes_from_application_config() {
    let fixture = Fixture::migrated();
    let application = config(
        fixture.database.path(),
        &[
            ("ADMIN_USER_IDS", "101,202"),
            ("PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS", "17"),
            ("PLAYER_TRIVIA_COOLDOWN_SECONDS", "23"),
            ("PLAYER_TRIVIA_INCLUDE_SPICY", "true"),
            ("MINIGAME_JC_DELTA_SCALE", "1.5"),
            ("PLAYER_TRIVIA_QUESTION_COUNT", "7"),
            ("PLAYER_TRIVIA_RECENT_DAYS", "11"),
            ("PLAYER_TRIVIA_REWARD_PER_CORRECT", "13"),
        ],
    );
    let runtime = PlayerTriviaRuntimeConfig::from_application_config(&application);
    assert_eq!(runtime.admin_user_ids, BTreeSet::from([101, 202]));
    assert_eq!(runtime.answer_timeout, Duration::from_secs(17));
    assert_eq!(runtime.cooldown_seconds, 23);
    assert!(runtime.include_spicy);
    assert_eq!(runtime.minigame_scale, 1.5);
    assert_eq!(runtime.question_count, 7);
    assert_eq!(runtime.recent_days, 11);
    assert_eq!(runtime.reward_per_correct, 13);
}

#[tokio::test]
async fn guild_guard_matches_python_copy_and_precedes_no_database_work() {
    let fixture = Fixture::migrated();
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .command_handler("playertrivia")
        .expect("handler")
        .handle(dm_command(USER), responder.clone())
        .await
        .expect("DM command");
    let captured = responder.captured.lock().expect("captured");
    assert_eq!(captured.immediate[0].content, GUILD_ONLY_MESSAGE);
    assert!(captured.immediate[0].ephemeral);
    assert!(captured.defers.is_empty());
    assert!(
        fixture
            .repository()
            .get_last_session_started(USER, GUILD)
            .expect("last session")
            .is_none()
    );
}

#[tokio::test]
async fn wrong_channel_charges_one_jc_and_returns_exact_gamba_copy() {
    let fixture = Fixture::migrated();
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord_with_gamba(false));
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .command_handler("playertrivia")
        .expect("handler")
        .handle(command(USER, None), responder.clone())
        .await
        .expect("wrong-channel command");
    let captured = responder.captured.lock().expect("captured");
    assert_eq!(captured.immediate[0].content, WRONG_CHANNEL_MESSAGE);
    assert!(captured.immediate[0].ephemeral);
    assert!(captured.defers.is_empty());
    drop(captured);
    assert_eq!(fixture.balance(USER), 2);
    assert!(
        fixture
            .repository()
            .get_last_session_started(USER, GUILD)
            .expect("last session")
            .is_none()
    );
}

#[test]
fn test_long_and_mention_options_remain_in_embed_not_button_labels() {
    let question = app::PlayerTriviaQuestion::new(
        "predictions:profit:101",
        "predictions",
        "Which resolved market did <@101> finish with a profit on?",
        [
            "Market #8 — Will Team Radiant win the next inhouse match?",
            "Market #14 — Will the game last longer than 45 minutes?",
            "<@202> — Trivia Player's server profile",
            "<@303> — Another Player's server profile",
        ]
        .map(str::to_owned),
        0,
        "Computed from resolved prediction markets.",
        false,
    )
    .expect("valid question");
    let response = question_response(
        42,
        &question,
        1,
        10,
        0,
        0,
        "Trivia Player",
        None,
        None,
        Duration::from_secs(20),
    );
    let choose = &response.embeds[0].fields[0].value;
    assert!(
        question
            .options
            .iter()
            .all(|option| choose.contains(option))
    );
    assert_eq!(
        response
            .components
            .iter()
            .flat_map(|row| row.buttons.iter().map(|button| button.label.as_str()))
            .collect::<Vec<_>>(),
        OPTION_LABELS
    );
}

#[tokio::test]
async fn test_command_starts_frozen_daily_set_with_spicy_disabled() {
    let fixture = Fixture::migrated();
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .command_handler("playertrivia")
        .expect("handler")
        .handle(command(USER, None), responder.clone())
        .await
        .expect("command");
    let session = latest_session(&fixture, USER);
    assert_eq!(session.question_count, 1);
    assert!(!session.questions[0].question.spicy);
    let response = &responder.captured.lock().expect("captured").followups[0];
    assert_eq!(response.embeds[0].fields[0].name, "Choose one");
    assert_eq!(response.embeds[0].fields[1].name, "Your daily set");
}

#[tokio::test]
async fn test_admin_bypasses_player_trivia_cooldown() {
    let fixture = Fixture::migrated();
    let prior = start_manual(&fixture, 1);
    fixture
        .repository()
        .finish_session(prior, "completed", unix_timestamp().unwrap())
        .expect("finish prior");
    let config = config(fixture.database.path(), &[("ADMIN_USER_IDS", "101")]);
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .command_handler("playertrivia")
        .unwrap()
        .handle(command(USER, None), responder.clone())
        .await
        .expect("admin command");
    assert_eq!(
        fixture
            .connection()
            .query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM player_trivia_sessions WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, USER],
                |row| row.get(0)
            )
            .unwrap(),
        2
    );
    assert!(responder.captured.lock().unwrap().immediate.is_empty());
}

#[tokio::test]
async fn manage_guild_permission_bypasses_player_trivia_cooldown() {
    let fixture = Fixture::migrated();
    let prior = start_manual(&fixture, 1);
    fixture
        .repository()
        .finish_session(prior, "completed", unix_timestamp().unwrap())
        .expect("finish prior");
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .command_handler("playertrivia")
        .expect("handler")
        .handle(
            command(USER, Some(MANAGE_GUILD_PERMISSION)),
            responder.clone(),
        )
        .await
        .expect("manage-guild command");
    assert!(responder.captured.lock().unwrap().immediate.is_empty());
    assert_eq!(
        fixture
            .connection()
            .query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM player_trivia_sessions WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, USER],
                |row| row.get(0),
            )
            .expect("session count"),
        2
    );
}

#[tokio::test]
async fn test_non_admin_remains_subject_to_player_trivia_cooldown() {
    let fixture = Fixture::migrated();
    let prior = start_manual(&fixture, 1);
    fixture
        .repository()
        .finish_session(prior, "completed", unix_timestamp().unwrap())
        .unwrap();
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .command_handler("playertrivia")
        .unwrap()
        .handle(command(USER, None), responder.clone())
        .await
        .expect("cooldown command");
    let response = responder.captured.lock().unwrap().immediate[0]
        .content
        .clone();
    assert!(response.starts_with("Player trivia is on cooldown! Your next set unlocks <t:"));
}

#[tokio::test]
async fn test_command_generates_and_tracks_separate_sets_per_player() {
    let fixture = Fixture::migrated();
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord());
    let handler = registry(&provider).command_handler("playertrivia").unwrap();
    for user_id in [USER, USER + 1] {
        handler
            .handle(
                command(user_id, None),
                Arc::new(RecordingResponder::default()),
            )
            .await
            .expect("independent command");
    }
    let first = latest_session(&fixture, USER);
    let second = latest_session(&fixture, USER + 1);
    assert_ne!(first.session_id, second.session_id);
    assert_eq!(first.discord_id, USER);
    assert_eq!(second.discord_id, USER + 1);
}

#[tokio::test]
async fn test_insufficient_bank_does_not_claim_cooldown() {
    let fixture = Fixture::migrated();
    let config = config(
        fixture.database.path(),
        &[("PLAYER_TRIVIA_QUESTION_COUNT", "100")],
    );
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .command_handler("playertrivia")
        .unwrap()
        .handle(command(USER, None), responder.clone())
        .await
        .expect("insufficient command");
    assert!(
        fixture
            .repository()
            .get_last_session_started(USER, GUILD)
            .unwrap()
            .is_none()
    );
    assert!(
        responder.captured.lock().unwrap().followups[0]
            .content
            .contains("No cooldown was used")
    );
}

#[tokio::test]
async fn test_first_message_failure_cancels_unanswered_session() {
    let fixture = Fixture::migrated();
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder {
        fail_followup: true,
        ..RecordingResponder::default()
    });
    registry(&provider)
        .command_handler("playertrivia")
        .unwrap()
        .handle(command(USER, None), responder)
        .await
        .expect("send failure command");
    assert!(
        fixture
            .repository()
            .active_session_for_player(USER, GUILD)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .repository()
            .get_last_session_started(USER, GUILD)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn test_wrong_answer_continues_to_next_question() {
    let fixture = Fixture::migrated();
    let session_id = start_manual(&fixture, 2);
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .component_handler("player_trivia:")
        .unwrap()
        .handle(
            component(format!("player_trivia:{session_id}:1:0"), USER),
            responder.clone(),
        )
        .await
        .expect("wrong answer");
    let session = fixture
        .repository()
        .get_session(session_id)
        .unwrap()
        .unwrap();
    assert_eq!(session.next_question().unwrap().question_number, 2);
    let response = &responder.captured.lock().unwrap().updates[0];
    assert_eq!(
        response.embeds[0].description.as_deref(),
        Some("Who won match question 2?")
    );
    assert_eq!(
        response.embeds[0].fields[1].name,
        "Previous question result"
    );
}

#[tokio::test]
async fn component_after_provider_restart_resumes_frozen_sqlite_session() {
    let fixture = Fixture::migrated();
    let session_id = start_manual(&fixture, 2);
    // The session predates this provider instance, matching a process restart:
    // no in-memory UI/session state is available to the new handler.
    let config = config(fixture.database.path(), &[]);
    let restarted_provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&restarted_provider)
        .component_handler("player_trivia:")
        .expect("component handler")
        .handle(
            component(format!("player_trivia:{session_id}:1:1"), USER),
            responder.clone(),
        )
        .await
        .expect("restarted component");

    let session = fixture
        .repository()
        .get_session(session_id)
        .expect("session read")
        .expect("session");
    assert_eq!(session.score, 1);
    assert_eq!(
        session.next_question().map(|row| row.question_number),
        Some(2)
    );
    assert_eq!(responder.captured.lock().unwrap().updates.len(), 1);
}

#[tokio::test]
async fn stale_component_after_restart_times_out_without_reward() {
    let fixture = Fixture::migrated();
    let now = unix_timestamp().expect("now");
    let session_id = fixture
        .repository()
        .try_start_session(USER, GUILD, &questions(1), now - 100, 0, true)
        .expect("start stale session")
        .expect("claimed");
    let config = config(
        fixture.database.path(),
        &[("PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS", "10")],
    );
    let restarted_provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&restarted_provider)
        .component_handler("player_trivia:")
        .expect("component handler")
        .handle(
            component(format!("player_trivia:{session_id}:1:1"), USER),
            responder.clone(),
        )
        .await
        .expect("stale component");

    let session = fixture
        .repository()
        .get_session(session_id)
        .expect("session read")
        .expect("session");
    assert_eq!(session.status, "timed_out");
    assert!(session.questions[0].selected_index.is_none());
    assert_eq!(fixture.balance(USER), 3);
    assert_eq!(
        responder.captured.lock().unwrap().updates[0].embeds[0]
            .title
            .as_deref(),
        Some("Player Trivia — Time's up!")
    );
}

#[tokio::test]
async fn test_last_correct_answer_shows_summary_and_ends_in_memory_session() {
    let fixture = Fixture::migrated();
    let session_id = start_manual(&fixture, 1);
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .component_handler("player_trivia:")
        .unwrap()
        .handle(
            component(format!("player_trivia:{session_id}:1:1"), USER),
            responder.clone(),
        )
        .await
        .expect("correct answer");
    let session = fixture
        .repository()
        .get_session(session_id)
        .unwrap()
        .unwrap();
    assert_eq!(session.status, "completed");
    assert_eq!(session.score, 1);
    let response = &responder.captured.lock().unwrap().updates[0];
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("Player Trivia — Complete!")
    );
    assert!(response.components.is_empty());
}

#[tokio::test]
async fn test_daily_event_runs_after_single_player_trivia_central_scale() {
    let fixture = Fixture::migrated();
    fixture.seed_event(r#"{"reward_multiplier":0.5}"#);
    let session_id = start_manual(&fixture, 1);
    let config = config(
        fixture.database.path(),
        &[
            ("ECONOMY_EVENTS_ENABLED", "true"),
            ("MINIGAME_JC_DELTA_SCALE", "2"),
            ("PLAYER_TRIVIA_REWARD_PER_CORRECT", "5"),
        ],
    );
    let provider = provider(&fixture, &config, discord());
    registry(&provider)
        .component_handler("player_trivia:")
        .unwrap()
        .handle(
            component(format!("player_trivia:{session_id}:1:1"), USER),
            Arc::new(RecordingResponder::default()),
        )
        .await
        .expect("scaled answer");
    let session = fixture
        .repository()
        .get_session(session_id)
        .unwrap()
        .unwrap();
    assert_eq!(session.jc_earned, 5);
    assert_eq!(fixture.balance(USER), 8);
}

#[tokio::test]
async fn test_daily_event_failure_does_not_strand_player_trivia_session() {
    let fixture = Fixture::migrated();
    fixture.seed_event("{");
    let session_id = start_manual(&fixture, 1);
    let config = config(
        fixture.database.path(),
        &[("ECONOMY_EVENTS_ENABLED", "true")],
    );
    let provider = provider(&fixture, &config, discord());
    registry(&provider)
        .component_handler("player_trivia:")
        .unwrap()
        .handle(
            component(format!("player_trivia:{session_id}:1:1"), USER),
            Arc::new(RecordingResponder::default()),
        )
        .await
        .expect("event fallback");
    let session = fixture
        .repository()
        .get_session(session_id)
        .unwrap()
        .unwrap();
    assert_eq!(session.status, "completed");
    assert_eq!(session.jc_earned, 1);
}

#[test]
fn test_missing_bot_or_player_trivia_event_service_is_neutral() {
    assert_eq!(scale_minigame_jc_delta(8.0, 1.0), 8);
}

#[tokio::test]
async fn test_other_player_is_rejected_ephemerally() {
    let fixture = Fixture::migrated();
    let session_id = start_manual(&fixture, 1);
    let config = config(fixture.database.path(), &[]);
    let provider = provider(&fixture, &config, discord());
    let responder = Arc::new(RecordingResponder::default());
    registry(&provider)
        .component_handler("player_trivia:")
        .unwrap()
        .handle(
            component(format!("player_trivia:{session_id}:1:0"), USER + 1),
            responder.clone(),
        )
        .await
        .expect("other user");
    let captured = responder.captured.lock().unwrap();
    assert_eq!(
        captured.immediate[0].content,
        "This isn't your player-trivia session!"
    );
    assert!(captured.immediate[0].ephemeral);
    assert!(
        fixture
            .repository()
            .get_session(session_id)
            .unwrap()
            .unwrap()
            .questions[0]
            .selected_index
            .is_none()
    );
}

#[tokio::test]
async fn test_timeout_finishes_run_and_keeps_daily_cooldown() {
    let fixture = Fixture::migrated();
    let config = config(
        fixture.database.path(),
        &[("PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS", "0")],
    );
    let discord = discord();
    let provider = provider(&fixture, &config, Arc::clone(&discord));
    registry(&provider)
        .command_handler("playertrivia")
        .unwrap()
        .handle(command(USER, None), Arc::new(RecordingResponder::default()))
        .await
        .expect("timeout command");
    let session_id: i64 = fixture
        .connection()
        .query_row(
            "SELECT session_id FROM player_trivia_sessions WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, USER],
            |row| row.get(0),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let timed_out = fixture
                .repository()
                .get_session(session_id)
                .expect("poll timed-out session")
                .is_some_and(|session| session.status == "timed_out");
            let edit_sent = !discord.edits.lock().expect("timeout edits").is_empty();
            if timed_out && edit_sent {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("timeout transition and Discord edit complete");
    let session = fixture
        .repository()
        .get_session(session_id)
        .unwrap()
        .unwrap();
    assert_eq!(session.status, "timed_out");
    assert!(
        fixture
            .repository()
            .get_last_session_started(USER, GUILD)
            .unwrap()
            .is_some()
    );
    let edits = discord.edits.lock().unwrap();
    assert_eq!(
        edits[0].1.embeds[0].title.as_deref(),
        Some("Player Trivia — Time's up!")
    );
    assert!(edits[0].1.components.is_empty());
}

#[test]
fn wrong_answer_response_continues_with_exact_previous_result_copy() {
    let previous = answer_result(&question(), false, 0);
    assert!(previous.starts_with("❌ Not this time. The answer was **B. Bravo**."));
    assert!(previous.contains("Computed from recorded inhouse matches."));
}
