use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::NamedTempFile;

use crate::discord_transport::DiscordMessageSnapshot;
use crate::registration::InteractionResponseError;
use crate::test_support::initialize_test_database;

use super::*;

const GUILD: u64 = 707;
const USER: u64 = 808;
const TOPIC_1: &str = "cama-000000000000000000000000000000000000000000000001";
const TOPIC_2: &str = "cama-000000000000000000000000000000000000000000000002";

fn migrated_database() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary database");
    initialize_test_database(file.path()).expect("migrate database");
    file
}

fn provider(path: &Path) -> PushNotificationRegistrationProvider {
    PushNotificationRegistrationProvider::new(path, Arc::new(RecordingDiscord::default()))
        .expect("build push notification provider")
}

fn provider_with_publisher(
    path: &Path,
    publisher: Arc<dyn PushPublisher>,
) -> PushNotificationRegistrationProvider {
    PushNotificationRegistrationProvider::with_test_publisher(
        path,
        Arc::new(RecordingDiscord::default()),
        publisher,
    )
}

fn provider_with_discord_and_publisher(
    path: &Path,
    discord: Arc<RecordingDiscord>,
    publisher: Arc<dyn PushPublisher>,
) -> PushNotificationRegistrationProvider {
    PushNotificationRegistrationProvider::with_test_publisher(path, discord, publisher)
}

#[test]
fn command_schema_registers_with_discord_valid_metadata() {
    let database = migrated_database();
    let provider =
        provider_with_publisher(database.path(), Arc::new(RecordingPublisher::default()));
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(&provider)
        .expect("register push notification provider");
    let registry = builder.build();
    let command = registry.commands().next().expect("notify command");

    assert_eq!(command.name, COMMAND_NAME);
    assert_eq!(command.description, COMMAND_DESCRIPTION);
    assert!(command.description.chars().count() <= 100);
    assert!(command.options.is_empty());
    assert_eq!(registry.commands().count(), 1);
    assert_eq!(registry.component_routes().len(), 1);
    assert_eq!(
        registry.component_routes()[0].custom_id_prefix,
        COMPONENT_PREFIX
    );
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublishedNotification {
    topic: String,
    title: String,
    message: String,
}

#[derive(Default)]
struct RecordingPublisher {
    published: StdMutex<Vec<PublishedNotification>>,
}

impl RecordingPublisher {
    fn published(&self) -> Vec<PublishedNotification> {
        self.published.lock().expect("published").clone()
    }

    fn wait_for_published(&self, expected: usize) -> Vec<PublishedNotification> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let published = self.published();
            if published.len() >= expected || Instant::now() >= deadline {
                return published;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[async_trait]
impl PushPublisher for RecordingPublisher {
    async fn publish(&self, topic: &str, title: &str, message: &str) -> Result<(), String> {
        self.published
            .lock()
            .expect("published")
            .push(PublishedNotification {
                topic: topic.to_owned(),
                title: title.to_owned(),
                message: message.to_owned(),
            });
        Ok(())
    }
}

#[derive(Default)]
struct SlowPublisher {
    active: AtomicUsize,
    maximum_active: AtomicUsize,
    completed: AtomicUsize,
}

#[async_trait]
impl PushPublisher for SlowPublisher {
    async fn publish(&self, _topic: &str, _title: &str, _message: &str) -> Result<(), String> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.completed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct RecordingDiscord {
    direct_messages: StdMutex<Vec<(u64, DiscordMessage)>>,
}

impl RecordingDiscord {
    fn direct_messages(&self) -> Vec<(u64, DiscordMessage)> {
        self.direct_messages
            .lock()
            .expect("direct messages")
            .clone()
    }

    fn wait_for_direct_messages(&self, expected: usize) -> Vec<(u64, DiscordMessage)> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let sent = self.direct_messages();
            if sent.len() >= expected || Instant::now() >= deadline {
                return sent;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[async_trait]
impl DiscordTransport for RecordingDiscord {
    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        Ok(None)
    }

    async fn send_message(
        &self,
        _channel_id: u64,
        _message: DiscordMessage,
    ) -> Result<crate::discord_transport::DiscordMessageReceipt, String> {
        Err("recording transport does not send channel messages".to_owned())
    }

    async fn edit_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn delete_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Err("recording transport does not create threads".to_owned())
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(
        &self,
        _thread_id: u64,
        _name: &str,
        _locked: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
        _user_id: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn clear_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn unpin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn send_direct_message(
        &self,
        user_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.direct_messages
            .lock()
            .expect("direct messages")
            .push((user_id, message));
        Ok(())
    }

    async fn guild_member(
        &self,
        _guild_id: u64,
        _user_id: u64,
    ) -> Result<Option<crate::discord_transport::DiscordGuildMemberSnapshot>, String> {
        Ok(None)
    }
}

#[derive(Default)]
struct RecordingResponder {
    responses: StdMutex<Vec<InteractionResponse>>,
    updates: StdMutex<Vec<InteractionResponse>>,
}

impl RecordingResponder {
    fn response(&self) -> InteractionResponse {
        self.responses.lock().expect("responses")[0].clone()
    }

    fn update_response(&self) -> InteractionResponse {
        self.updates.lock().expect("updates")[0].clone()
    }
}

#[async_trait]
impl InteractionResponder for RecordingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.responses.lock().expect("responses").push(response);
        Ok(())
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.responses.lock().expect("responses").push(response);
        Ok(())
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.updates.lock().expect("updates").push(response);
        Ok(())
    }

    async fn show_modal(
        &self,
        _modal: crate::registration::InteractionModal,
    ) -> Result<(), InteractionResponseError> {
        panic!("push notification tests do not expect a modal")
    }
}

fn command_request() -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: COMMAND_NAME.to_owned(),
        user_id: USER,
        user_display_name: "Notify User".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(909),
        member_permissions: None,
        options: Vec::new(),
    }
}

fn component_request(custom_id: &str) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: 2,
        custom_id: custom_id.to_owned(),
        user_id: USER,
        user_display_name: "Notify User".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(909),
        member_permissions: None,
        values: Vec::new(),
    }
}

fn buttons(response: &InteractionResponse) -> Vec<&InteractionButton> {
    response
        .components
        .iter()
        .flat_map(|row| &row.buttons)
        .collect()
}

#[tokio::test]
async fn command_without_a_guild_is_rejected_as_server_only() {
    let database = migrated_database();
    let provider = provider(database.path());
    let responder = Arc::new(RecordingResponder::default());
    let request = InteractionRequest::Command {
        interaction_id: 1,
        name: COMMAND_NAME.to_owned(),
        user_id: USER,
        user_display_name: "Notify User".to_owned(),
        guild_id: None,
        channel_id: None,
        member_permissions: None,
        options: Vec::new(),
    };

    provider
        .handler
        .handle(
            request,
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("DM command handled");

    let response = responder.response();
    assert_eq!(response.content, SERVER_ONLY_MESSAGE);
    assert!(response.ephemeral);
    // Nothing may be stored under the absent-guild sentinel.
    let repository = PushNotificationRepository::new(database.path());
    assert_eq!(
        repository
            .get_config(USER as i64, Some(0))
            .expect("read config"),
        None
    );
}

#[tokio::test]
async fn component_without_a_guild_is_rejected_without_writing_config() {
    let database = migrated_database();
    let provider = provider(database.path());
    let responder = Arc::new(RecordingResponder::default());
    let request = InteractionRequest::Component {
        interaction_id: 2,
        custom_id: BUTTON_SET.to_owned(),
        user_id: USER,
        user_display_name: "Notify User".to_owned(),
        guild_id: None,
        channel_id: None,
        member_permissions: None,
        values: Vec::new(),
    };

    provider
        .handler
        .handle(
            request,
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("DM component handled");

    assert_eq!(responder.update_response().content, SERVER_ONLY_MESSAGE);
    let repository = PushNotificationRepository::new(database.path());
    assert_eq!(
        repository
            .get_config(USER as i64, Some(0))
            .expect("read config"),
        None
    );
}

#[tokio::test]
async fn command_with_no_target_offers_set_button_and_dm_toggles() {
    let database = migrated_database();
    let provider = provider(database.path());
    let responder = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            command_request(),
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("command handled");
    let response = responder.response();
    assert!(response.content.contains("No ntfy topic configured"));
    let buttons = buttons(&response);
    let ids = buttons
        .iter()
        .map(|button| button.custom_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            BUTTON_SET,
            BUTTON_TOGGLE_READYCHECK_DM,
            BUTTON_TOGGLE_MATCH_STARTED_DM,
        ]
    );
    // No target and nothing enabled yet: no test/unsubscribe actions to show.
    assert!(!ids.contains(&BUTTON_TEST));
    assert!(!ids.contains(&BUTTON_UNSUBSCRIBE));
}

#[tokio::test]
async fn create_topic_button_persists_high_entropy_target_and_enables_both_ntfy_kinds() {
    let database = migrated_database();
    let provider = provider(database.path());
    let responder = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            component_request(BUTTON_SET),
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("create topic button handled");

    let repository = PushNotificationRepository::new(database.path());
    let config = repository
        .get_config(USER as i64, Some(GUILD as i64))
        .expect("read config")
        .expect("target persisted");
    let topic = &config.target.as_ref().expect("ntfy target").topic;
    assert!(topic.starts_with(TOPIC_PREFIX));
    assert_eq!(topic.len(), TOPIC_PREFIX.len() + TOPIC_RANDOM_BYTES * 2);
    assert!(
        topic[TOPIC_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert!(config.readycheck_enabled);
    assert!(config.match_started_enabled);
    assert!(!config.dm_readycheck_enabled);
    assert!(!config.dm_match_started_enabled);

    let response = responder.update_response();
    assert!(response.content.contains(DEFAULT_NTFY_SERVER));
    assert!(response.content.contains(topic));
    assert!(response.content.contains("Keep this topic private"));
}

#[tokio::test]
async fn regenerate_topic_replaces_the_secret() {
    let database = migrated_database();
    let provider = provider(database.path());
    let repository = PushNotificationRepository::new(database.path());
    let first = generate_topic().expect("first topic");
    repository
        .set_target(USER as i64, Some(GUILD as i64), &first, 1)
        .expect("seed target");

    provider
        .handler
        .handle(
            component_request(BUTTON_SET),
            Arc::new(RecordingResponder::default()),
        )
        .await
        .expect("regenerate topic");

    let second = repository
        .get_config(USER as i64, Some(GUILD as i64))
        .expect("read config")
        .expect("target persisted")
        .target
        .expect("ntfy target")
        .topic;
    assert_ne!(first, second);
}

#[tokio::test]
async fn toggle_button_flips_one_ntfy_kind_independently() {
    let database = migrated_database();
    let provider = provider(database.path());
    let repository = PushNotificationRepository::new(database.path());
    repository
        .set_target(USER as i64, Some(GUILD as i64), TOPIC_1, 1)
        .expect("seed target");

    let responder = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            component_request(BUTTON_TOGGLE_READYCHECK_NTFY),
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("toggle handled");

    let config = repository
        .get_config(USER as i64, Some(GUILD as i64))
        .expect("read config")
        .expect("target still present");
    assert!(!config.readycheck_enabled);
    assert!(config.match_started_enabled);
    let response = responder.update_response();
    let readycheck_button = buttons(&response)
        .into_iter()
        .find(|button| button.custom_id == BUTTON_TOGGLE_READYCHECK_NTFY)
        .expect("readycheck ntfy toggle button present");
    assert_eq!(readycheck_button.label, "Readycheck (ntfy): OFF");
}

#[tokio::test]
async fn dm_toggle_creates_a_preference_row_without_any_ntfy_topic() {
    let database = migrated_database();
    let provider = provider(database.path());
    let repository = PushNotificationRepository::new(database.path());
    assert_eq!(
        repository
            .get_config(USER as i64, Some(GUILD as i64))
            .expect("read config"),
        None
    );

    let responder = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            component_request(BUTTON_TOGGLE_MATCH_STARTED_DM),
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("DM toggle handled");

    let config = repository
        .get_config(USER as i64, Some(GUILD as i64))
        .expect("read config")
        .expect("DM preference row created");
    assert!(config.target.is_none());
    assert!(config.dm_match_started_enabled);
    assert!(!config.dm_readycheck_enabled);

    let response = responder.update_response();
    let button = buttons(&response)
        .into_iter()
        .find(|button| button.custom_id == BUTTON_TOGGLE_MATCH_STARTED_DM)
        .expect("match started DM toggle button present");
    assert_eq!(button.label, "Match Started (DM): ON");
    // No ntfy target yet: the ntfy toggle row must stay hidden.
    assert!(
        !buttons(&response)
            .iter()
            .any(|button| button.custom_id == BUTTON_TOGGLE_READYCHECK_NTFY)
    );
    // A DM preference now exists, so Send Test and Unsubscribe reappear.
    assert!(
        buttons(&response)
            .iter()
            .any(|button| button.custom_id == BUTTON_TEST)
    );
    assert!(
        buttons(&response)
            .iter()
            .any(|button| button.custom_id == BUTTON_UNSUBSCRIBE)
    );
}

#[tokio::test]
async fn unsubscribe_button_deletes_target_and_dm_preferences() {
    let database = migrated_database();
    let provider = provider(database.path());
    let repository = PushNotificationRepository::new(database.path());
    repository
        .set_target(USER as i64, Some(GUILD as i64), TOPIC_1, 1)
        .expect("seed target");
    repository
        .set_enabled(
            USER as i64,
            Some(GUILD as i64),
            PushNotificationKind::Readycheck,
            PushNotificationChannel::DirectMessage,
            true,
            1,
        )
        .expect("seed DM preference");

    let responder = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            component_request(BUTTON_UNSUBSCRIBE),
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("unsubscribe handled");

    assert_eq!(
        repository
            .get_config(USER as i64, Some(GUILD as i64))
            .expect("read config"),
        None
    );
    assert!(
        responder
            .update_response()
            .content
            .contains("No ntfy topic configured")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_readycheck_launched_delivers_only_to_ntfy_enabled_subscribers() {
    let database = migrated_database();
    let publisher = Arc::new(RecordingPublisher::default());
    let provider = provider_with_publisher(database.path(), publisher.clone());
    let repository = PushNotificationRepository::new(database.path());
    repository
        .set_target(1, Some(GUILD as i64), TOPIC_1, 1)
        .expect("seed subscriber");
    repository
        .set_target(2, Some(GUILD as i64), TOPIC_2, 1)
        .expect("seed non-subscriber");
    repository
        .set_enabled(
            2,
            Some(GUILD as i64),
            PushNotificationKind::Readycheck,
            PushNotificationChannel::Ntfy,
            false,
            2,
        )
        .expect("disable readycheck for player 2");

    provider
        .hooks()
        .notify_readycheck_launched(GUILD, [1_u64, 2_u64]);

    let published = publisher.wait_for_published(1);
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].topic, TOPIC_1);
    assert_eq!(published[0].title, READYCHECK_TITLE);
    assert_eq!(published[0].message, READYCHECK_MESSAGE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_match_started_uses_the_independent_match_started_ntfy_toggle() {
    let database = migrated_database();
    let publisher = Arc::new(RecordingPublisher::default());
    let provider = provider_with_publisher(database.path(), publisher.clone());
    let repository = PushNotificationRepository::new(database.path());
    repository
        .set_target(1, Some(GUILD as i64), TOPIC_1, 1)
        .expect("seed subscriber");
    repository
        .set_enabled(
            1,
            Some(GUILD as i64),
            PushNotificationKind::Readycheck,
            PushNotificationChannel::Ntfy,
            false,
            2,
        )
        .expect("disable readycheck only");

    provider
        .hooks()
        .notify_match_started(GUILD, &BTreeSet::from([1_u64]));

    let published = publisher.wait_for_published(1);
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].topic, TOPIC_1);
    assert_eq!(published[0].title, MATCH_STARTED_TITLE);
    assert_eq!(published[0].message, MATCH_STARTED_MESSAGE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_readycheck_launched_also_delivers_dm_to_enabled_subscribers() {
    let database = migrated_database();
    let publisher = Arc::new(RecordingPublisher::default());
    let discord = Arc::new(RecordingDiscord::default());
    let provider = provider_with_discord_and_publisher(database.path(), discord.clone(), publisher);
    let repository = PushNotificationRepository::new(database.path());
    repository
        .set_enabled(
            1,
            Some(GUILD as i64),
            PushNotificationKind::Readycheck,
            PushNotificationChannel::DirectMessage,
            true,
            1,
        )
        .expect("seed DM subscriber");
    repository
        .set_enabled(
            2,
            Some(GUILD as i64),
            PushNotificationKind::Readycheck,
            PushNotificationChannel::DirectMessage,
            false,
            1,
        )
        .expect("seed DM non-subscriber");

    provider
        .hooks()
        .notify_readycheck_launched(GUILD, [1_u64, 2_u64]);

    let sent = discord.wait_for_direct_messages(1);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, 1);
    assert!(sent[0].1.response.content.contains(READYCHECK_TITLE));
    assert!(sent[0].1.response.content.contains(READYCHECK_MESSAGE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_match_started_delivers_to_both_ntfy_and_dm_when_a_player_enables_both() {
    let database = migrated_database();
    let publisher = Arc::new(RecordingPublisher::default());
    let discord = Arc::new(RecordingDiscord::default());
    let provider =
        provider_with_discord_and_publisher(database.path(), discord.clone(), publisher.clone());
    let repository = PushNotificationRepository::new(database.path());
    repository
        .set_target(1, Some(GUILD as i64), TOPIC_1, 1)
        .expect("seed ntfy target");
    repository
        .set_enabled(
            1,
            Some(GUILD as i64),
            PushNotificationKind::MatchStarted,
            PushNotificationChannel::DirectMessage,
            true,
            1,
        )
        .expect("also enable DM for the same player");

    provider
        .hooks()
        .notify_match_started(GUILD, &BTreeSet::from([1_u64]));

    let published = publisher.wait_for_published(1);
    assert_eq!(published.len(), 1);
    let sent = discord.wait_for_direct_messages(1);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, 1);
}

#[tokio::test]
async fn test_delivery_is_rate_limited_per_user_and_guild() {
    let database = migrated_database();
    let publisher = Arc::new(RecordingPublisher::default());
    let provider = provider_with_publisher(database.path(), publisher.clone());
    PushNotificationRepository::new(database.path())
        .set_target(USER as i64, Some(GUILD as i64), TOPIC_1, 1)
        .expect("seed target");

    let first = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            component_request(BUTTON_TEST),
            Arc::clone(&first) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("first test delivery");
    let second = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            component_request(BUTTON_TEST),
            Arc::clone(&second) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("rate-limited test delivery");

    assert_eq!(publisher.published().len(), 1);
    assert!(
        first
            .update_response()
            .content
            .contains("notification sent")
    );
    assert!(second.update_response().content.contains("rate-limited"));
}

#[tokio::test]
async fn test_delivery_covers_both_channels_when_both_are_active() {
    let database = migrated_database();
    let publisher = Arc::new(RecordingPublisher::default());
    let discord = Arc::new(RecordingDiscord::default());
    let provider =
        provider_with_discord_and_publisher(database.path(), discord.clone(), publisher.clone());
    let repository = PushNotificationRepository::new(database.path());
    repository
        .set_target(USER as i64, Some(GUILD as i64), TOPIC_1, 1)
        .expect("seed target");
    repository
        .set_enabled(
            USER as i64,
            Some(GUILD as i64),
            PushNotificationKind::Readycheck,
            PushNotificationChannel::DirectMessage,
            true,
            1,
        )
        .expect("seed DM preference");

    let responder = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            component_request(BUTTON_TEST),
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("test delivery");

    assert_eq!(publisher.published().len(), 1);
    assert_eq!(discord.direct_messages().len(), 1);
    let content = responder.update_response().content;
    assert!(content.contains("ntfy test notification sent"));
    assert!(content.contains("DM test notification sent"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_fanout_is_parallel_but_globally_bounded() {
    let database = migrated_database();
    let publisher = Arc::new(SlowPublisher::default());
    let provider = provider_with_publisher(database.path(), publisher.clone());
    let repository = PushNotificationRepository::new(database.path());
    let users = (1_i64..=8).collect::<Vec<_>>();
    for user in &users {
        repository
            .set_target(*user, Some(GUILD as i64), &format!("cama-{user:048x}"), 1)
            .expect("seed fanout target");
    }

    provider.hooks().notify_readycheck_launched(
        GUILD,
        users
            .iter()
            .map(|user| u64::try_from(*user).expect("test user ID")),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while publisher.completed.load(Ordering::SeqCst) < users.len() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(publisher.completed.load(Ordering::SeqCst), users.len());
    let maximum_active = publisher.maximum_active.load(Ordering::SeqCst);
    assert!(maximum_active > 1, "fanout should not be sequential");
    assert!(maximum_active <= DELIVERY_CONCURRENCY);
}
