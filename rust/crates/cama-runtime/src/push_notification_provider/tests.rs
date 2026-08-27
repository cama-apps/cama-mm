use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex as StdMutex;

use tempfile::NamedTempFile;

use crate::registration::InteractionResponseError;
use crate::test_support::initialize_test_database;

use super::*;

const GUILD: u64 = 707;
const USER: u64 = 808;

fn migrated_database() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary database");
    initialize_test_database(file.path()).expect("migrate database");
    file
}

fn provider(path: &Path) -> PushNotificationRegistrationProvider {
    PushNotificationRegistrationProvider::new(path).expect("build push notification provider")
}

#[derive(Default)]
struct RecordingResponder {
    responses: StdMutex<Vec<InteractionResponse>>,
    updates: StdMutex<Vec<InteractionResponse>>,
    modals: StdMutex<Vec<InteractionModal>>,
}

impl RecordingResponder {
    fn response(&self) -> InteractionResponse {
        self.responses.lock().expect("responses")[0].clone()
    }

    fn update_response(&self) -> InteractionResponse {
        self.updates.lock().expect("updates")[0].clone()
    }

    fn modal(&self) -> InteractionModal {
        self.modals.lock().expect("modals")[0].clone()
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

    async fn show_modal(&self, modal: InteractionModal) -> Result<(), InteractionResponseError> {
        self.modals.lock().expect("modals").push(modal);
        Ok(())
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

fn modal_request(topic: &str, server: &str) -> InteractionRequest {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(TOPIC_FIELD.to_owned(), topic.to_owned());
    if !server.is_empty() {
        fields.insert(SERVER_FIELD.to_owned(), server.to_owned());
    }
    InteractionRequest::Modal {
        interaction_id: 3,
        custom_id: MODAL_CUSTOM_ID.to_owned(),
        user_id: USER,
        guild_id: Some(GUILD),
        channel_id: Some(909),
        member_permissions: None,
        fields,
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
    assert!(response.content.contains("No ntfy target configured"));
    assert_eq!(response.components.len(), 1);
    assert_eq!(response.components[0].buttons.len(), 1);
    assert_eq!(response.components[0].buttons[0].custom_id, BUTTON_SET);
}

#[tokio::test]
async fn set_target_button_opens_modal() {
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
        .expect("set button handled");
    let modal = responder.modal();
    assert_eq!(modal.custom_id, MODAL_CUSTOM_ID);
    assert_eq!(modal.inputs.len(), 2);
}

#[tokio::test]
async fn modal_submit_persists_target_and_enables_both_kinds() {
    let database = migrated_database();
    let provider = provider(database.path());
    let responder = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            modal_request("my-secret-topic", ""),
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("modal handled");

    let repository = PushNotificationRepository::new(database.path());
    let config = repository
        .get_config(USER as i64, Some(GUILD as i64))
        .expect("read config")
        .expect("target persisted");
    assert_eq!(config.target.topic, "my-secret-topic");
    assert_eq!(config.target.server, DEFAULT_NTFY_SERVER);
    assert!(config.readycheck_enabled);
    assert!(config.lobby_enabled);

    let response = responder.response();
    assert!(response.content.contains("my-secret-topic"));
}

#[tokio::test]
async fn modal_submit_rejects_topic_with_slash() {
    let database = migrated_database();
    let provider = provider(database.path());
    let responder = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            modal_request("bad/topic", ""),
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("modal handled");

    let repository = PushNotificationRepository::new(database.path());
    assert_eq!(
        repository
            .get_config(USER as i64, Some(GUILD as i64))
            .expect("read config"),
        None
    );
    assert!(responder.response().content.contains("must not contain"));
}

#[tokio::test]
async fn toggle_button_flips_one_kind_independently() {
    let database = migrated_database();
    let provider = provider(database.path());
    let repository = PushNotificationRepository::new(database.path());
    repository
        .set_target(USER as i64, Some(GUILD as i64), DEFAULT_NTFY_SERVER, "t", 1)
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
    assert!(config.lobby_enabled);
    let response = responder.update_response();
    assert!(response.content.contains("Readycheck: OFF"));
}

#[tokio::test]
async fn clear_button_deletes_target() {
    let database = migrated_database();
    let provider = provider(database.path());
    let repository = PushNotificationRepository::new(database.path());
    repository
        .set_target(USER as i64, Some(GUILD as i64), DEFAULT_NTFY_SERVER, "t", 1)
        .expect("seed target");

    let responder = Arc::new(RecordingResponder::default());
    provider
        .handler
        .handle(
            component_request(BUTTON_CLEAR),
            Arc::clone(&responder) as Arc<dyn InteractionResponder>,
        )
        .await
        .expect("clear handled");

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
            .contains("No ntfy target configured")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_readycheck_launched_delivers_only_to_enabled_subscribers() {
    let database = migrated_database();
    let provider = provider(database.path());
    let repository = PushNotificationRepository::new(database.path());
    let server = LoopbackNtfyServer::start();
    repository
        .set_target(1, Some(GUILD as i64), &server.base_url, "topic-1", 1)
        .expect("seed subscriber");
    repository
        .set_target(2, Some(GUILD as i64), &server.base_url, "topic-2", 1)
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

    let requests = server.wait_for_requests(1);
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("POST /topic-1"));
}

struct LoopbackNtfyServer {
    base_url: String,
    requests: Arc<StdMutex<Vec<String>>>,
}

impl LoopbackNtfyServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback ntfy server");
        let base_url = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let captured = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    return;
                };
                let mut buffer = [0_u8; 512];
                if let Ok(read) = stream.read(&mut buffer) {
                    let text = String::from_utf8_lossy(&buffer[..read]).into_owned();
                    if let Some(line) = text.lines().next() {
                        captured.lock().expect("requests").push(line.to_owned());
                    }
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        Self { base_url, requests }
    }

    fn wait_for_requests(&self, expected: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let requests = self.requests.lock().expect("requests").clone();
            if requests.len() >= expected || std::time::Instant::now() >= deadline {
                return requests;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
