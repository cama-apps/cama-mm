use super::*;
use crate::gateway::GatewayIntentProfile;
use crate::registration::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionButton, InteractionHandlerError,
    InteractionRequest, InteractionResponder, Registry, RegistryBuilder,
};
use async_trait::async_trait;
use serenity::all::{ApplicationId, Message};
use serenity::http::HttpBuilder;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

struct WireTestHandler;

#[derive(Default)]
struct RecordingInteractionResponder {
    events: Mutex<Vec<&'static str>>,
    reject_initial: AtomicBool,
    initial_delay_ms: AtomicU64,
}

impl RecordingInteractionResponder {
    fn record(&self, event: &'static str) -> Result<(), InteractionResponseError> {
        self.events.lock().expect("events lock").push(event);
        if self.reject_initial.load(Ordering::Acquire)
            && matches!(event, "respond" | "defer" | "update" | "show_modal")
        {
            Err(InteractionResponseError::new("Unknown interaction"))
        } else {
            Ok(())
        }
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.lock().expect("events lock").clone()
    }

    async fn record_initial(&self, event: &'static str) -> Result<(), InteractionResponseError> {
        let delay = self.initial_delay_ms.load(Ordering::Acquire);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        self.record(event)
    }
}

#[async_trait]
impl InteractionResponder for RecordingInteractionResponder {
    async fn respond(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.record_initial("respond").await
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.record_initial("defer").await
    }

    async fn followup(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.record("followup")
    }

    async fn show_modal(&self, _modal: InteractionModal) -> Result<(), InteractionResponseError> {
        self.record_initial("show_modal").await
    }

    async fn update(&self, _response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.record_initial("update").await
    }

    async fn edit_original(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.record("edit_original")
    }
}

struct DelayedUpdateHandler;

#[async_trait]
impl InteractionHandler for DelayedUpdateHandler {
    async fn handle(
        &self,
        _request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        tokio::time::sleep(Duration::from_millis(15)).await;
        responder
            .update(InteractionResponse::message("complete"))
            .await
            .map_err(|error| error.to_string().into())
    }
}

struct DelayedModalHandler;

#[async_trait]
impl InteractionHandler for DelayedModalHandler {
    fn acknowledgement_policy(
        &self,
        _request: &InteractionRequest,
    ) -> InteractionAcknowledgementPolicy {
        InteractionAcknowledgementPolicy::Modal
    }

    async fn handle(
        &self,
        _request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        tokio::time::sleep(Duration::from_millis(15)).await;
        responder
            .show_modal(InteractionModal {
                custom_id: "test:modal".to_owned(),
                title: "Test".to_owned(),
                inputs: Vec::new(),
            })
            .await
            .map_err(|error| error.to_string().into())
    }
}

struct SilentComponentHandler;

#[async_trait]
impl InteractionHandler for SilentComponentHandler {
    async fn handle(
        &self,
        _request: InteractionRequest,
        _responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        Ok(())
    }
}

fn component_request() -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: 1,
        custom_id: "test:component".to_owned(),
        user_id: 2,
        user_display_name: "Tester".to_owned(),
        guild_id: Some(3),
        channel_id: Some(4),
        member_permissions: None,
        values: Vec::new(),
    }
}

#[tokio::test]
async fn delayed_component_update_is_automatically_deferred_then_edited() {
    let inner = Arc::new(RecordingInteractionResponder::default());
    let responder = coordinated_responder(inner.clone());
    run_component_handler_with_deadline(
        Arc::new(DelayedUpdateHandler),
        component_request(),
        responder,
        Duration::from_millis(1),
    )
    .await;

    assert_eq!(inner.events(), ["defer", "edit_original"]);
}

#[tokio::test]
async fn modal_policy_never_runs_the_automatic_defer_watchdog() {
    let inner = Arc::new(RecordingInteractionResponder::default());
    let responder = coordinated_responder(inner.clone());
    run_component_handler_with_deadline(
        Arc::new(DelayedModalHandler),
        component_request(),
        responder,
        Duration::from_millis(1),
    )
    .await;

    assert_eq!(inner.events(), ["show_modal"]);
}

#[tokio::test]
async fn concurrent_initial_callbacks_send_only_one_discord_response() {
    let inner = Arc::new(RecordingInteractionResponder::default());
    inner.initial_delay_ms.store(10, Ordering::Release);
    let responder = coordinated_responder(inner.clone());
    let (defer, update) = tokio::join!(
        responder.defer(false),
        responder.update(InteractionResponse::message("complete")),
    );
    defer.expect("defer coordination");
    update.expect("update coordination");

    let events = inner.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(**event, "defer" | "update"))
            .count(),
        1
    );
}

#[tokio::test]
async fn rejected_initial_response_is_not_retried_by_failure_reporting() {
    let inner = Arc::new(RecordingInteractionResponder::default());
    inner.reject_initial.store(true, Ordering::Release);
    let responder = coordinated_responder(inner.clone());
    responder
        .respond(InteractionResponse::message("late"))
        .await
        .expect_err("initial response is rejected");

    report_handler_failure(&responder)
        .await
        .expect("rejected response is not retried");
    assert_eq!(inner.events(), ["respond"]);
}

#[tokio::test]
async fn successful_silent_handler_receives_stale_control_response() {
    let inner = Arc::new(RecordingInteractionResponder::default());
    let responder = coordinated_responder(inner.clone());
    run_component_handler_with_deadline(
        Arc::new(SilentComponentHandler),
        component_request(),
        responder,
        Duration::from_secs(1),
    )
    .await;

    assert_eq!(inner.events(), ["respond"]);
}

#[test]
fn configured_gamba_channel_id_is_accepted() {
    assert!(configured_gamba_location_matches(Some(42), 42, None));
    assert!(!configured_gamba_location_matches(Some(42), 43, None));
}

#[test]
fn configured_gamba_thread_parent_is_accepted() {
    assert!(configured_gamba_location_matches(Some(42), 43, Some(42)));
    assert!(!configured_gamba_location_matches(Some(42), 43, Some(44)));
}

struct CapturedCommandRequest {
    request_line: String,
    body: Vec<u8>,
}

struct CommandWireFixture {
    proxy: String,
    requests: mpsc::Receiver<CapturedCommandRequest>,
    server: JoinHandle<()>,
}

#[async_trait]
impl InteractionHandler for WireTestHandler {
    async fn handle(
        &self,
        _request: InteractionRequest,
        _responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        Ok(())
    }
}

fn command_registry(global_sync_enabled: bool) -> Arc<Registry> {
    let mut builder = RegistryBuilder::default();
    builder
        .command(CommandSpec {
            name: "health".to_owned(),
            description: "Runtime health".to_owned(),
            options: vec![
                CommandOptionSpec::new("mode", "Health response mode", CommandOptionKind::String)
                    .required(true),
            ],
            handler: Arc::new(WireTestHandler),
        })
        .expect("register wire-test command");
    if global_sync_enabled {
        builder.enable_global_command_sync();
    }
    Arc::new(builder.build())
}

fn wire_test_handler(registry: Arc<Registry>) -> SerenityHandler {
    let (events, _) = tokio::sync::broadcast::channel(8);
    SerenityHandler {
        registry,
        events,
        observers: GatewayEventObservers::default(),
        global_interaction_hooks: None,
        raw_reaction_observers: RawReactionObservers::default(),
        discord_transport: None,
        bot_user_id: AtomicU64::new(0),
        guild_ids: Arc::new(RwLock::new(BTreeSet::new())),
    }
}

fn command_wire_fixture(status: u16, response_body: &'static str) -> CommandWireFixture {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local Discord command proxy");
    let proxy = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("local Discord command proxy address")
    );
    let (sender, receiver) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("accept Discord command synchronization request");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = stream
                .read(&mut chunk)
                .expect("read Discord command request");
            assert!(read > 0, "command proxy received an incomplete request");
            request.extend_from_slice(&chunk[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .expect("command synchronization content length");
        while request.len() < header_end + content_length {
            let read = stream
                .read(&mut chunk)
                .expect("read command synchronization body");
            assert!(read > 0, "command proxy received a truncated request body");
            request.extend_from_slice(&chunk[..read]);
        }
        let request_line = headers
            .lines()
            .next()
            .expect("command synchronization request line")
            .to_owned();
        sender
            .send(CapturedCommandRequest {
                request_line,
                body: request[header_end..header_end + content_length].to_vec(),
            })
            .expect("deliver captured command request");

        let reason = match status {
            200 => "OK",
            500 => "Internal Server Error",
            _ => "Fixture Response",
        };
        write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len(),
            )
            .expect("write command proxy response");
    });
    CommandWireFixture {
        proxy,
        requests: receiver,
        server,
    }
}

#[test]
fn python_profile_maps_to_default_plus_exact_privileged_intents() {
    let actual = serenity_intents(GatewayIntentProfile::python_parity());
    let expected = GatewayIntents::non_privileged()
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_PRESENCES
        | GatewayIntents::MESSAGE_CONTENT;

    assert_eq!(actual, expected);
    assert_eq!(
        actual & GatewayIntents::privileged(),
        GatewayIntents::privileged()
    );
    assert_eq!(actual.bits(), expected.bits());
}

#[tokio::test]
async fn ready_sync_serializes_the_enabled_composed_registry_over_discord_wire() {
    let fixture = command_wire_fixture(200, "[]");
    let http = HttpBuilder::new("test-token")
        .application_id(ApplicationId::new(42))
        .proxy(fixture.proxy)
        .ratelimiter_disabled(true)
        .build();
    let handler = wire_test_handler(command_registry(true));

    let outcome = handler.synchronize_global_commands(&http).await;
    let request = fixture
        .requests
        .recv_timeout(Duration::from_secs(2))
        .expect("enabled command sync reaches the Discord proxy");
    fixture.server.join().expect("command proxy completes");

    assert_eq!(outcome.command_count, 1);
    assert!(outcome.synchronized);
    assert_eq!(outcome.error, None);
    assert_eq!(
        request.request_line,
        "PUT /api/v10/applications/42/commands HTTP/1.1"
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&request.body).expect("serialized command tree is JSON");
    assert_eq!(payload.as_array().map(Vec::len), Some(1));
    assert_eq!(payload[0]["name"], "health");
    assert_eq!(payload[0]["description"], "Runtime health");
    assert_eq!(payload[0]["options"][0]["name"], "mode");
    assert_eq!(
        payload[0]["options"][0]["description"],
        "Health response mode"
    );
    assert_eq!(payload[0]["options"][0]["type"], 3);
    assert_eq!(payload[0]["options"][0]["required"], true);
    assert_eq!(
        outcome,
        GlobalCommandSyncOutcome {
            command_count: 1,
            synchronized: true,
            error: None,
        }
    );
}

#[tokio::test]
async fn ready_sync_disabled_preserves_discord_without_issuing_http() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind disabled command-sync proxy");
    listener
        .set_nonblocking(true)
        .expect("make disabled command-sync proxy nonblocking");
    let proxy = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("disabled command-sync proxy address")
    );
    let http = HttpBuilder::new("test-token")
        .application_id(ApplicationId::new(42))
        .proxy(proxy)
        .ratelimiter_disabled(true)
        .build();
    let handler = wire_test_handler(command_registry(false));

    let outcome = handler.synchronize_global_commands(&http).await;

    assert_eq!(
        outcome,
        GlobalCommandSyncOutcome {
            command_count: 1,
            synchronized: false,
            error: None,
        }
    );
    assert_eq!(
        handler
            .synchronize_global_commands(&http)
            .await
            .lifecycle_event(0),
        LifecycleEvent::CommandsRegistered {
            command_count: 1,
            component_route_count: 0,
            synchronized: false,
        }
    );
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[tokio::test]
async fn ready_sync_rest_failure_reports_unsynchronized_lifecycle_outcome() {
    let fixture = command_wire_fixture(
        500,
        r#"{"message":"fixture command REST rejection","code":50000}"#,
    );
    let http = HttpBuilder::new("test-token")
        .application_id(ApplicationId::new(42))
        .proxy(fixture.proxy)
        .ratelimiter_disabled(true)
        .build();
    let handler = wire_test_handler(command_registry(true));

    let outcome = handler.synchronize_global_commands(&http).await;
    let request = fixture
        .requests
        .recv_timeout(Duration::from_secs(2))
        .expect("failed command sync reaches the Discord proxy");
    fixture
        .server
        .join()
        .expect("failed command proxy completes");

    assert_eq!(
        request.request_line,
        "PUT /api/v10/applications/42/commands HTTP/1.1"
    );
    assert!(
        !request.body.is_empty(),
        "failed request still carried command schemas"
    );
    assert_eq!(outcome.command_count, 1);
    assert!(!outcome.synchronized);
    assert!(
        outcome
            .error
            .as_deref()
            .is_some_and(|error| error.contains("fixture command REST rejection"))
    );
    assert_eq!(
        outcome.lifecycle_event(0),
        LifecycleEvent::CommandsRegistered {
            command_count: 1,
            component_route_count: 0,
            synchronized: false,
        }
    );
}

#[test]
fn dig_public_send_fallback_requires_definitive_discord_rejection() {
    assert_eq!(
        dig_public_send_failure_kind(Some(403)),
        DigPublicSendFailureKind::Rejected
    );
    assert_eq!(
        dig_public_send_failure_kind(Some(404)),
        DigPublicSendFailureKind::Rejected
    );
    assert_eq!(
        dig_public_send_failure_kind(None),
        DigPublicSendFailureKind::Ambiguous
    );
    assert_eq!(
        dig_public_send_failure_kind(Some(408)),
        DigPublicSendFailureKind::Ambiguous
    );
    assert_eq!(
        dig_public_send_failure_kind(Some(429)),
        DigPublicSendFailureKind::Ambiguous
    );
    assert_eq!(
        dig_public_send_failure_kind(Some(500)),
        DigPublicSendFailureKind::Ambiguous
    );
}

#[test]
fn typed_profile_can_exclude_non_parity_intents() {
    let actual = serenity_intents(GatewayIntentProfile {
        guilds: false,
        guild_members: true,
        guild_presences: false,
        guild_messages: true,
        guild_message_reactions: true,
        direct_messages: false,
        message_content: true,
    });
    assert_eq!(
        actual,
        GatewayIntents::GUILD_MEMBERS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::GUILD_MESSAGE_REACTIONS
            | GatewayIntents::MESSAGE_CONTENT
    );
    assert!(!actual.contains(GatewayIntents::AUTO_MODERATION_CONFIGURATION));
    assert!(!actual.contains(GatewayIntents::GUILD_PRESENCES));
}

#[test]
fn serenity_member_conversion_uses_server_nickname_not_global_display_name() {
    let mut member = Member::default();
    member.guild_id = GuildId::new(42);
    member.user.id = UserId::new(7);
    member.user.global_name = Some("Global Display Name".to_owned());
    member.nick = None;

    assert_eq!(member_server_nickname(&member), None);
    assert_eq!(gateway_member(&member), GatewayMember::new(42, 7, None));

    member.nick = Some("Server Nickname".to_owned());
    assert_eq!(
        member_server_nickname(&member),
        Some("Server Nickname".to_owned())
    );
    assert_eq!(
        gateway_member(&member),
        GatewayMember::new(42, 7, Some("Server Nickname".to_owned()))
    );
}

#[test]
fn nickname_snapshot_includes_only_requested_cached_members() {
    let mut requested = Member::default();
    requested.guild_id = GuildId::new(42);
    requested.user.id = UserId::new(7);
    requested.nick = Some("Requested Nickname".to_owned());
    let mut unrelated = Member::default();
    unrelated.guild_id = GuildId::new(42);
    unrelated.user.id = UserId::new(8);
    unrelated.nick = Some("Unrelated Nickname".to_owned());
    let members = HashMap::from([
        (requested.user.id, requested),
        (unrelated.user.id, unrelated),
    ]);

    assert_eq!(
        requested_member_server_nicknames(&members, &[7]),
        DiscordGuildMemberServerNicknames::from([(7, Some("Requested Nickname".to_owned()))])
    );
}

#[test]
fn allowed_mentions_none_and_user_allowlist_map_to_discord_builders() {
    let none = serde_json::to_value(
        serenity_allowed_mentions(&InteractionAllowedMentions::None).expect("explicit none policy"),
    )
    .expect("serialize allowed mentions");
    assert_eq!(none["parse"], serde_json::json!([]));
    assert_eq!(none["users"], serde_json::json!([]));
    assert_eq!(none["roles"], serde_json::json!([]));

    let users = serde_json::to_value(
        serenity_allowed_mentions(&InteractionAllowedMentions::Users(vec![7, 9]))
            .expect("explicit user policy"),
    )
    .expect("serialize allowed mentions");
    assert_eq!(users["parse"], serde_json::json!([]));
    assert_eq!(users["users"], serde_json::json!(["7", "9"]));
    assert_eq!(users["roles"], serde_json::json!([]));
    assert!(serenity_allowed_mentions(&InteractionAllowedMentions::Default).is_none());
}

#[test]
fn thread_reply_builder_serializes_parent_reference() {
    let request = channel_response(InteractionResponse::message("reply"))
        .reference_message((ChannelId::new(77_003), MessageId::new(77_004)));
    let serialized = serde_json::to_value(request).expect("serialize thread reply");
    assert_eq!(
        serialized["message_reference"]["channel_id"],
        serde_json::json!("77003")
    );
    assert_eq!(
        serialized["message_reference"]["message_id"],
        serde_json::json!("77004")
    );
}

#[test]
fn component_defer_keeps_update_ack_while_thinking_uses_public_type_five() {
    let ordinary = serde_json::to_value(component_defer_response(false, false))
        .expect("serialize ordinary component defer");
    let thinking = serde_json::to_value(component_defer_response(true, false))
        .expect("serialize public thinking defer");
    let private_thinking = serde_json::to_value(component_defer_response(true, true))
        .expect("serialize private thinking defer");

    assert_eq!(ordinary["type"], serde_json::json!(6));
    assert_eq!(thinking["type"], serde_json::json!(5));
    assert_eq!(thinking["data"]["flags"], serde_json::json!(0));
    assert_eq!(private_thinking["type"], serde_json::json!(5));
    assert_eq!(private_thinking["data"]["flags"], serde_json::json!(64));
}

#[test]
fn interaction_embed_thumbnail_maps_to_discord_thumbnail() {
    let embed =
        InteractionEmbed::titled("Economy Event").thumbnail("https://cdn.example/spell.png");
    let serialized = serde_json::to_value(serenity_embed(&embed)).expect("serialize Discord embed");

    assert_eq!(
        serialized["thumbnail"]["url"],
        "https://cdn.example/spell.png"
    );
    assert!(serialized.get("image").is_none());
}

#[test]
fn interaction_embed_footer_icon_maps_to_discord_footer_icon() {
    let embed = InteractionEmbed::titled("Dig result")
        .footer("Community Mine")
        .footer_icon("attachment://pickaxe.png");
    let serialized = serde_json::to_value(serenity_embed(&embed)).expect("serialize Discord embed");

    assert_eq!(serialized["footer"]["text"], "Community Mine");
    assert_eq!(serialized["footer"]["icon_url"], "attachment://pickaxe.png");
}

#[test]
fn attachment_upload_and_edit_replacement_preserve_typed_bytes() {
    let mut attachment = InteractionAttachment::bytes("wheel.gif", vec![0, 1, 2, 255]);
    attachment.description = Some("wheel animation".to_owned());
    let upload = serenity_attachment(&attachment);
    assert_eq!(upload.filename, "wheel.gif");
    assert_eq!(upload.description.as_deref(), Some("wheel animation"));
    assert_eq!(upload.data, vec![0, 1, 2, 255]);

    let uploaded = serde_json::to_value(response_message(
        InteractionResponse::message("spinning").attachment(attachment.clone()),
    ))
    .expect("serialize attachment upload");
    assert_eq!(uploaded["attachments"][0]["filename"], "wheel.gif");

    let serialized = serde_json::to_value(edit_response(
        InteractionResponse::message("settled").attachment(attachment),
    ))
    .expect("serialize attachment replacement");
    assert_eq!(serialized["attachments"][0]["id"], serde_json::json!(0));
    assert_eq!(serialized["attachments"][0]["filename"], "wheel.gif");
    assert_eq!(
        serialized["attachments"][0]["description"],
        "wheel animation"
    );
}

#[test]
fn auto_component_only_channel_edit_preserves_existing_body_and_uploads() {
    let response =
        InteractionResponse::message("").action_row(InteractionActionRow::buttons(vec![
            InteractionButton::new("wheel:keep", "Keep result"),
        ]));
    assert_eq!(
        response.attachment_policy,
        InteractionAttachmentPolicy::Auto
    );
    assert!(is_component_only_edit(&response));

    let serialized = serde_json::to_value(edit_receipt_channel_message(response))
        .expect("serialize automatic component-only channel edit");
    assert!(serialized.get("content").is_none());
    assert!(serialized.get("embeds").is_none());
    assert!(serialized.get("attachments").is_none());
    assert_eq!(
        serialized["components"][0]["components"][0]["custom_id"],
        "wheel:keep"
    );
}

#[test]
fn channel_fallback_preserve_replaces_view_without_clearing_content_or_attachments() {
    let response = InteractionResponse::message("")
        .embed(InteractionEmbed::titled("Challenge declined"))
        .action_row(InteractionActionRow::buttons(vec![InteractionButton::new(
            "pet:brawl:7:closed",
            "Closed",
        )]))
        .preserve_attachments();
    let serialized = serde_json::to_value(edit_receipt_channel_message(response))
        .expect("serialize attachment-preserving channel fallback edit");

    assert!(serialized.get("content").is_none());
    assert!(serialized.get("attachments").is_none());
    assert_eq!(serialized["embeds"][0]["title"], "Challenge declined");
    assert_eq!(
        serialized["components"][0]["components"][0]["custom_id"],
        "pet:brawl:7:closed"
    );
}

#[test]
fn component_only_explicit_clear_serializes_empty_attachments() {
    let response = InteractionResponse::message("")
        .action_row(InteractionActionRow::buttons(vec![InteractionButton::new(
            "pet:brawl:7:closed",
            "Closed",
        )]))
        .clear_attachments();

    assert!(!is_component_only_edit(&response));
    let update = serde_json::to_value(CreateInteractionResponse::UpdateMessage(response_message(
        response.clone(),
    )))
    .expect("serialize component update clear");
    assert_eq!(update["data"]["attachments"], serde_json::json!([]));
    for serialized in [
        serde_json::to_value(edit_response(response.clone()))
            .expect("serialize original response clear"),
        serde_json::to_value(edit_followup_message(response.clone()))
            .expect("serialize followup response clear"),
        serde_json::to_value(edit_receipt_channel_message(response))
            .expect("serialize channel fallback clear"),
    ] {
        assert_eq!(serialized["attachments"], serde_json::json!([]));
        assert_eq!(
            serialized["components"][0]["components"][0]["custom_id"],
            "pet:brawl:7:closed"
        );
    }
}

#[test]
fn auto_component_only_followup_uses_attachment_omitting_raw_payload() {
    let response =
        InteractionResponse::message("").action_row(InteractionActionRow::buttons(vec![
            InteractionButton::new("wheel:keep", "Keep result"),
        ]));

    assert_eq!(
        response.attachment_policy,
        InteractionAttachmentPolicy::Auto
    );
    assert!(is_component_only_edit(&response));

    let regular_builder = serde_json::to_value(edit_followup_message(response.clone()))
        .expect("serialize Serenity followup edit");
    assert_eq!(regular_builder["attachments"], serde_json::json!([]));

    let preserving = serde_json::to_value(component_only_interaction_edit(response.clone()))
        .expect("serialize attachment-preserving followup edit");
    assert!(preserving.get("attachments").is_none());
    assert_eq!(
        preserving["components"][0]["components"][0]["custom_id"],
        "wheel:keep"
    );

    let update = serde_json::to_value(attachment_preserving_interaction_callback(response))
        .expect("serialize automatic component update");
    assert!(update["data"].get("attachments").is_none());
    assert!(update["data"].get("embeds").is_none());
}

#[test]
fn normal_followup_edit_can_omit_attachments_to_preserve_existing_upload() {
    let response = InteractionResponse::message("settled")
        .embed(InteractionEmbed::titled("Challenge declined"))
        .action_row(InteractionActionRow::buttons(vec![InteractionButton::new(
            "pet:brawl:7:accept",
            "Accept",
        )]))
        .with_user_mentions(vec![77])
        .preserve_attachments();
    let serialized = serde_json::to_value(attachment_preserving_message(response))
        .expect("serialize attachment-preserving normal followup edit");

    assert_eq!(serialized["content"], "settled");
    assert_eq!(serialized["embeds"][0]["title"], "Challenge declined");
    assert_eq!(
        serialized["components"][0]["components"][0]["custom_id"],
        "pet:brawl:7:accept"
    );
    assert_eq!(
        serialized["allowed_mentions"]["users"],
        serde_json::json!(["77"])
    );
    assert!(serialized.get("attachments").is_none());
    assert!(serialized.get("flags").is_none());
}

#[test]
fn normal_followup_edit_retains_explicit_clear_semantics() {
    let serialized = serde_json::to_value(edit_followup_message(
        InteractionResponse::message("replace body")
            .embed(InteractionEmbed::titled("Clear media"))
            .clear_attachments(),
    ))
    .expect("serialize explicit attachment clear");

    assert_eq!(serialized["attachments"], serde_json::json!([]));
}

#[test]
fn component_update_preserve_payload_omits_attachments_and_flags() {
    let response = InteractionResponse::message("")
        .embed(InteractionEmbed::titled("Challenge declined"))
        .preserve_attachments();
    let serialized = serde_json::to_value(attachment_preserving_interaction_callback(response))
        .expect("serialize attachment-preserving component update");

    assert_eq!(serialized["type"], 7);
    assert_eq!(
        serialized["data"]["embeds"][0]["title"],
        "Challenge declined"
    );
    assert_eq!(serialized["data"]["components"], serde_json::json!([]));
    assert!(serialized["data"].get("attachments").is_none());
    assert!(serialized["data"].get("flags").is_none());

    let component_only = serde_json::to_value(attachment_preserving_interaction_callback(
        InteractionResponse::message("")
            .action_row(InteractionActionRow::buttons(vec![InteractionButton::new(
                "pet:brawl:7:accept",
                "Accept",
            )]))
            .preserve_attachments(),
    ))
    .expect("serialize component-only preserve update");
    assert!(component_only["data"].get("embeds").is_none());
}

#[tokio::test]
async fn component_only_followup_transport_preserves_existing_media() {
    let listener =
        TcpListener::bind("127.0.0.1:0").expect("bind local Discord HTTP capture server");
    let proxy = format!("http://{}", listener.local_addr().expect("capture address"));
    let request_body = Arc::new(Mutex::new(None));
    let captured_body = Arc::clone(&request_body);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept Discord HTTP request");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("read Discord HTTP request");
            assert!(read > 0, "capture server received an incomplete request");
            request.extend_from_slice(&chunk[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length:")
                    .or_else(|| line.strip_prefix("content-length:"))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .expect("JSON request content length");
        while request.len() < header_end + content_length {
            let read = stream.read(&mut chunk).expect("read JSON request body");
            assert!(read > 0, "capture server received a truncated request body");
            request.extend_from_slice(&chunk[..read]);
        }
        *captured_body.lock().expect("capture body lock") =
            Some(request[header_end..header_end + content_length].to_vec());

        let response = serde_json::to_vec(&Message::default()).expect("serialize HTTP reply");
        write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .expect("write Discord HTTP response headers");
        stream
            .write_all(&response)
            .expect("write Discord HTTP response body");
    });

    let http = HttpBuilder::new("test-token")
        .application_id(ApplicationId::new(42))
        .proxy(proxy)
        .ratelimiter_disabled(true)
        .build();
    let response =
        InteractionResponse::message("").action_row(InteractionActionRow::buttons(vec![
            InteractionButton::new("wheel:timeout", "Timed out"),
        ]));
    edit_component_only_followup(&http, "interaction-token", MessageId::new(99), response)
        .await
        .expect("component-only followup edit succeeds");
    server.join().expect("capture server completes");

    let body = request_body
        .lock()
        .expect("capture body lock")
        .clone()
        .expect("captured edit request body");
    let serialized: serde_json::Value = serde_json::from_slice(&body).expect("JSON edit body");
    assert!(serialized.get("content").is_none());
    assert!(serialized.get("embeds").is_none());
    assert!(serialized.get("attachments").is_none());
    assert_eq!(
        serialized["components"][0]["components"][0]["custom_id"],
        "wheel:timeout"
    );
}

#[tokio::test]
async fn component_only_original_transport_preserves_existing_media() {
    let listener =
        TcpListener::bind("127.0.0.1:0").expect("bind local Discord HTTP capture server");
    let proxy = format!("http://{}", listener.local_addr().expect("capture address"));
    let request_body = Arc::new(Mutex::new(None));
    let captured_body = Arc::clone(&request_body);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept Discord HTTP request");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).expect("read Discord HTTP request");
            assert!(read > 0, "capture server received an incomplete request");
            request.extend_from_slice(&chunk[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length:")
                    .or_else(|| line.strip_prefix("content-length:"))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .expect("JSON request content length");
        while request.len() < header_end + content_length {
            let read = stream.read(&mut chunk).expect("read JSON request body");
            assert!(read > 0, "capture server received a truncated request body");
            request.extend_from_slice(&chunk[..read]);
        }
        *captured_body.lock().expect("capture body lock") =
            Some(request[header_end..header_end + content_length].to_vec());

        let response = serde_json::to_vec(&Message::default()).expect("serialize HTTP reply");
        write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .expect("write Discord HTTP response headers");
        stream
            .write_all(&response)
            .expect("write Discord HTTP response body");
    });

    let http = HttpBuilder::new("test-token")
        .application_id(ApplicationId::new(42))
        .proxy(proxy)
        .ratelimiter_disabled(true)
        .build();
    let response =
        InteractionResponse::message("").action_row(InteractionActionRow::buttons(vec![
            InteractionButton::new("wheel:original-timeout", "Timed out"),
        ]));
    edit_component_only_original(&http, "interaction-token", response)
        .await
        .expect("component-only original edit succeeds");
    server.join().expect("capture server completes");

    let body = request_body
        .lock()
        .expect("capture body lock")
        .clone()
        .expect("captured edit request body");
    let serialized: serde_json::Value = serde_json::from_slice(&body).expect("JSON edit body");
    assert!(serialized.get("content").is_none());
    assert!(serialized.get("embeds").is_none());
    assert!(serialized.get("attachments").is_none());
    assert_eq!(
        serialized["components"][0]["components"][0]["custom_id"],
        "wheel:original-timeout"
    );
}

#[test]
fn auto_non_component_followup_edit_retains_historical_media_clear() {
    let response = InteractionResponse::message("timeout complete");
    assert_eq!(
        response.attachment_policy,
        InteractionAttachmentPolicy::Auto
    );
    assert!(!is_component_only_edit(&response));
    let serialized = serde_json::to_value(edit_followup_message(response))
        .expect("serialize automatic non-component media clear");

    assert_eq!(serialized["embeds"], serde_json::json!([]));
    assert_eq!(serialized["attachments"], serde_json::json!([]));
    assert_eq!(serialized["content"], "timeout complete");
}

#[test]
fn followup_edit_with_new_media_replaces_existing_media() {
    let serialized = serde_json::to_value(edit_followup_message(
        InteractionResponse::message("replacement")
            .embed(InteractionEmbed::titled("Replacement"))
            .attachment(InteractionAttachment::bytes(
                "replacement.txt",
                b"new".to_vec(),
            )),
    ))
    .expect("serialize media replacement");

    assert_eq!(serialized["embeds"].as_array().map(Vec::len), Some(1));
    assert_eq!(serialized["attachments"][0]["filename"], "replacement.txt");
    assert_eq!(serialized["content"], "replacement");
}

#[test]
fn typed_autocomplete_option_enables_discord_autocomplete() {
    let option = CommandOptionSpec::new("hero_name", "The hero name", CommandOptionKind::String)
        .autocomplete();
    let serialized =
        serde_json::to_value(create_option(&option)).expect("serialize command option");
    assert_eq!(serialized["autocomplete"], true);
    assert_eq!(serialized["type"], u8::from(CommandOptionType::String));
}

#[test]
fn raw_reaction_emoji_preserves_unicode_and_custom_identity() {
    assert_eq!(
        gateway_reaction_emoji(&ReactionType::Unicode("⚔️".to_owned())),
        RawReactionEmoji::unicode("⚔️")
    );
    assert_eq!(
        gateway_reaction_emoji(&ReactionType::Custom {
            animated: true,
            id: serenity::all::EmojiId::new(42),
            name: Some("jopacoin".to_owned()),
        }),
        RawReactionEmoji::custom(42, "jopacoin", true)
    );
}

#[test]
fn embed_only_edit_can_preserve_existing_message_content() {
    let preserving = DiscordMessage::silent(
        InteractionResponse::message("").embed(InteractionEmbed::titled("Updated")),
    )
    .preserving_content();
    let preserving =
        serde_json::to_value(edit_channel_response(preserving)).expect("serialize preserving edit");
    assert!(preserving.get("content").is_none());

    let replacing = DiscordMessage::silent(InteractionResponse::message("replacement"));
    let replacing =
        serde_json::to_value(edit_channel_response(replacing)).expect("serialize replacing edit");
    assert_eq!(replacing["content"], "replacement");
}

#[test]
fn active_match_thread_edit_locks_without_archiving() {
    let serialized = serde_json::to_value(thread_lifecycle_edit(
        "🔒 All You Can Feed Shuffled - Awaiting Results",
        false,
        true,
    ))
    .expect("serialize thread edit");

    assert_eq!(serialized["archived"], false);
    assert_eq!(serialized["locked"], true);
    assert_eq!(
        serialized["name"],
        "🔒 All You Can Feed Shuffled - Awaiting Results"
    );
}

#[test]
fn streaming_bonus_follows_voice_go_live_not_twitch_presence() {
    // 10 is screen-sharing, 20 is in voice without Go Live, 30 is not in voice
    // at all (a Twitch "Streaming" rich presence looks exactly like this).
    let self_stream = |user_id: u64| match user_id {
        10 => Some(true),
        20 => Some(false),
        _ => None,
    };

    assert_eq!(
        go_live_member_ids(&[10, 20, 30], self_stream),
        BTreeSet::from([10])
    );
    // Members outside the requested set never earn the bonus.
    assert!(go_live_member_ids(&[20, 30], self_stream).is_empty());
}

/// Serve one canned HTTP reply and return the proxy base URL plus the server
/// thread, so a transport helper can be driven without a live gateway.
fn canned_discord_reply(status: &'static str, body: String) -> (String, JoinHandle<()>) {
    let listener =
        TcpListener::bind("127.0.0.1:0").expect("bind local Discord HTTP capture server");
    let proxy = format!("http://{}", listener.local_addr().expect("capture address"));
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept Discord HTTP request");
        let mut chunk = [0_u8; 4096];
        let mut request = Vec::new();
        loop {
            let read = stream.read(&mut chunk).expect("read Discord HTTP request");
            assert!(read > 0, "capture server received an incomplete request");
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .expect("write Discord HTTP response headers");
        stream
            .write_all(body.as_bytes())
            .expect("write Discord HTTP response body");
    });
    (proxy, server)
}

#[tokio::test]
async fn guild_name_falls_back_to_http_when_the_gateway_cache_is_cold() {
    let (proxy, server) = canned_discord_reply(
        "200 OK",
        serde_json::json!({
            "id": "42",
            "name": "Camaraderous",
            "owner_id": "7",
            "roles": [],
            "emojis": [],
            "features": [],
            "verification_level": 0,
            "default_message_notifications": 0,
            "explicit_content_filter": 0,
            "mfa_level": 0,
            "nsfw_level": 0,
            "premium_tier": 0,
            "system_channel_flags": 0,
            "stickers": [],
            "premium_progress_bar_enabled": false,
            "preferred_locale": "en-US"
        })
        .to_string(),
    );
    let http = HttpBuilder::new("test-token")
        .application_id(ApplicationId::new(42))
        .proxy(proxy)
        .ratelimiter_disabled(true)
        .build();

    let name = fetch_guild_name(&http, GuildId::new(42))
        .await
        .expect("guild lookup succeeds");
    server.join().expect("capture server completes");
    assert_eq!(
        name,
        Some("Camaraderous".to_owned()),
        "a cold cache must not silently degrade the notice to unqualified copy"
    );
}

#[tokio::test]
async fn guild_name_reports_a_missing_guild_as_absent_rather_than_an_error() {
    let (proxy, server) = canned_discord_reply(
        "404 Not Found",
        serde_json::json!({"code": 10004, "message": "Unknown Guild"}).to_string(),
    );
    let http = HttpBuilder::new("test-token")
        .application_id(ApplicationId::new(42))
        .proxy(proxy)
        .ratelimiter_disabled(true)
        .build();

    let name = fetch_guild_name(&http, GuildId::new(42))
        .await
        .expect("a missing guild is not a transport failure");
    server.join().expect("capture server completes");
    assert_eq!(name, None);
}
