use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::NamedTempFile;

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
    PushNotificationRegistrationProvider::new(path).expect("build push notification provider")
}

fn provider_with_publisher(
    path: &Path,
    publisher: Arc<dyn PushPublisher>,
) -> PushNotificationRegistrationProvider {
    PushNotificationRegistrationProvider::with_test_publisher(path, publisher)
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

#[tokio::test]
async fn command_with_no_target_offers_set_button() {
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
    assert_eq!(response.components.len(), 1);
    assert_eq!(response.components[0].buttons.len(), 1);
    assert_eq!(response.components[0].buttons[0].custom_id, BUTTON_SET);
}

#[tokio::test]
async fn create_topic_button_persists_high_entropy_target_and_enables_both_kinds() {
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
    assert!(config.target.topic.starts_with(TOPIC_PREFIX));
    assert_eq!(
        config.target.topic.len(),
        TOPIC_PREFIX.len() + TOPIC_RANDOM_BYTES * 2
    );
    assert!(
        config.target.topic[TOPIC_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert!(config.readycheck_enabled);
    assert!(config.match_started_enabled);

    let response = responder.update_response();
    assert!(response.content.contains(DEFAULT_NTFY_SERVER));
    assert!(response.content.contains(&config.target.topic));
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
        .topic;
    assert_ne!(first, second);
}

#[tokio::test]
async fn toggle_button_flips_one_kind_independently() {
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
            component_request(BUTTON_TOGGLE_READYCHECK),
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
    let readycheck_button = response
        .components
        .iter()
        .flat_map(|row| &row.buttons)
        .find(|button| button.custom_id == BUTTON_TOGGLE_READYCHECK)
        .expect("readycheck toggle button present");
    assert_eq!(readycheck_button.label, "Readycheck: OFF");
}

#[tokio::test]
async fn unsubscribe_button_deletes_target() {
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
async fn notify_readycheck_launched_delivers_only_to_enabled_subscribers() {
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
async fn notify_match_started_uses_the_independent_match_started_toggle() {
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
