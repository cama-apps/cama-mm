use std::path::PathBuf;
use std::sync::Mutex;

use cama_app::blame_luke_media::{
    BLAME_LUKE_FINAL_HOLD_MS, BLAME_LUKE_FRAME_COUNT, BLAME_LUKE_HEIGHT, BLAME_LUKE_WIDTH,
};
use cama_db::blame_luke::BLAME_LUKE_COST;
use cama_db::schema_manager::initialize_or_migrate;
use gif::{ColorOutput, DecodeOptions};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::*;
use crate::registration::{InteractionAllowedMentions, InteractionResponseError, Registry};

const USER: i64 = 42;
const GUILD: i64 = 123;
const CHANNEL: u64 = 999;

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
}

impl Fixture {
    fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("cama.db");
        initialize_or_migrate(&path).expect("run Rust migration authority");
        Self {
            _directory: directory,
            path,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open fixture database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production foreign-key mode");
        connection
    }

    fn register(&self, user_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, 'blame-luke-player', ?3)",
                params![user_id, GUILD, balance],
            )
            .expect("register Blame Luke fixture player");
        self.connection()
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear opening balance ledger");
    }

    fn balance(&self, user_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![user_id, GUILD],
                |row| row.get(0),
            )
            .expect("read Blame Luke balance")
    }

    fn ledger(&self) -> Vec<(i64, String, Option<String>)> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT delta, source, related_id
                 FROM economy_ledger_entries ORDER BY ledger_id",
            )
            .expect("prepare Blame Luke ledger query");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query Blame Luke ledger")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect Blame Luke ledger")
    }
}

#[derive(Default)]
struct CapturingResponder {
    immediate: Mutex<Vec<InteractionResponse>>,
    thinking_defers: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
    fail_thinking_defer: bool,
    fail_followup: bool,
}

impl CapturingResponder {
    fn failing_defer() -> Self {
        Self {
            fail_thinking_defer: true,
            ..Self::default()
        }
    }

    fn failing_followup() -> Self {
        Self {
            fail_followup: true,
            ..Self::default()
        }
    }
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.immediate.lock().expect("immediate").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.thinking_defers.lock().expect("defers").push(ephemeral);
        Ok(())
    }

    async fn defer_thinking(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.thinking_defers
            .lock()
            .expect("thinking defers")
            .push(ephemeral);
        if self.fail_thinking_defer {
            Err(InteractionResponseError::new("expired component"))
        } else {
            Ok(())
        }
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        if self.fail_followup {
            Err(InteractionResponseError::new("Discord delivery failed"))
        } else {
            Ok(())
        }
    }
}

struct FixedSelector(usize);

impl BlameLukeReasonSelector for FixedSelector {
    fn select(&self, _candidate_count: usize) -> usize {
        self.0
    }
}

struct FixedRenderer {
    result: Mutex<Result<Vec<u8>, String>>,
    calls: Mutex<Vec<usize>>,
}

impl FixedRenderer {
    fn bytes(bytes: &[u8]) -> Self {
        Self {
            result: Mutex::new(Ok(bytes.to_vec())),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn error(message: &str) -> Self {
        Self {
            result: Mutex::new(Err(message.to_owned())),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl BlameLukeRenderPort for FixedRenderer {
    fn render(&self, selected_reason_index: usize) -> Result<Vec<u8>, String> {
        self.calls
            .lock()
            .expect("render calls")
            .push(selected_reason_index);
        self.result.lock().expect("render result").clone()
    }
}

struct ErrorWallet;

impl BlameLukeWalletPort for ErrorWallet {
    fn charge(
        &self,
        _user_id: i64,
        _guild_id: i64,
        _selected_reason_index: usize,
    ) -> Result<BlameLukeChargeOutcome, String> {
        Err("database unavailable".to_owned())
    }

    fn refund(&self, _user_id: i64, _guild_id: i64) -> Result<(), String> {
        Ok(())
    }
}

fn provider_with(
    fixture: &Fixture,
    selected_index: usize,
    renderer: Arc<dyn BlameLukeRenderPort>,
) -> BlameLukeRegistrationProvider {
    BlameLukeRegistrationProvider::with_ports(
        Arc::new(BlameLukeRepository::new(&fixture.path)),
        Arc::new(FixedSelector(selected_index)),
        renderer,
    )
}

fn registry(provider: &BlameLukeRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(provider)
        .expect("register Blame Luke provider");
    builder.build()
}

fn command_request(guild_id: Option<u64>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "blameluke".to_owned(),
        user_id: USER as u64,
        user_display_name: "Launcher".to_owned(),
        guild_id,
        channel_id: Some(CHANNEL),
        member_permissions: None,
        options: Vec::new(),
    }
}

fn component_request(user_id: i64, display_name: &str) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: 2,
        custom_id: BLAME_LUKE_COMPONENT_ID.to_owned(),
        user_id: user_id as u64,
        user_display_name: display_name.to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        values: Vec::new(),
    }
}

#[test]
fn registration_matches_command_and_restart_safe_component_contract() {
    let fixture = Fixture::migrated();
    let registry = registry(&BlameLukeRegistrationProvider::new(&fixture.path));
    let commands = registry.commands().collect::<Vec<_>>();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "blameluke");
    assert_eq!(commands[0].description, "Discover why this is Luke's fault");
    assert!(commands[0].options.is_empty());
    assert!(
        registry
            .component_handler(BLAME_LUKE_COMPONENT_ID)
            .is_some()
    );
}

#[tokio::test]
async fn test_command_posts_public_button_anyone_can_use() {
    let fixture = Fixture::migrated();
    let handler = registry(&BlameLukeRegistrationProvider::new(&fixture.path))
        .command_handler("blameluke")
        .expect("Blame Luke command handler");
    let responder = Arc::new(CapturingResponder::default());
    handler
        .handle(command_request(Some(GUILD as u64)), responder.clone())
        .await
        .expect("post launcher");

    let immediate = responder.immediate.lock().expect("immediate");
    let response = &immediate[0];
    assert!(!response.ephemeral);
    assert_eq!(response.allowed_mentions, InteractionAllowedMentions::None);
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("Ministry of Accountability")
    );
    assert!(
        response.embeds[0]
            .description
            .as_deref()
            .expect("launcher description")
            .contains("impartial blame apparatus")
    );
    assert!(
        !response.embeds[0]
            .description
            .as_deref()
            .unwrap()
            .contains("JC")
    );
    let button = &response.components[0].buttons[0];
    assert_eq!(button.custom_id, BLAME_LUKE_COMPONENT_ID);
    assert_eq!(button.label, "Blame Luke");
    assert_eq!(button.emoji.as_deref(), Some("📌"));
    assert_eq!(button.style, InteractionButtonStyle::Danger);
}

#[tokio::test]
async fn test_button_charges_clicker_and_posts_public_result_on_migrated_sqlite() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    let provider = provider_with(&fixture, 3, Arc::new(NativeBlameLukeRenderer));
    let handler = registry(&provider)
        .component_handler(BLAME_LUKE_COMPONENT_ID)
        .expect("persistent Blame Luke handler");
    let responder = Arc::new(CapturingResponder::default());
    handler
        .handle(
            component_request(USER, "Not the launcher"),
            responder.clone(),
        )
        .await
        .expect("run investigation");

    assert_eq!(fixture.balance(USER), 100 - BLAME_LUKE_COST);
    assert_eq!(
        fixture.ledger(),
        [(-10, "blame_luke".to_owned(), Some("3".to_owned()))]
    );
    assert_eq!(*responder.thinking_defers.lock().expect("defers"), [false]);
    let followups = responder.followups.lock().expect("followups");
    let result = &followups[0];
    assert!(!result.ephemeral);
    assert_eq!(result.allowed_mentions, InteractionAllowedMentions::None);
    assert_eq!(
        result.embeds[0].title.as_deref(),
        Some("📌 Finding of the Ministry of Accountability")
    );
    assert_eq!(
        result.embeds[0].description.as_deref(),
        Some("**Luke deleted his messages**")
    );
    assert_eq!(
        result.embeds[0].footer.as_deref(),
        Some("Filed by Not the launcher")
    );
    assert_eq!(
        result.embeds[0].image_url.as_deref(),
        Some("attachment://blame_luke.gif")
    );
    assert_eq!(result.attachments[0].filename, "blame_luke.gif");

    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::Indexed);
    let mut decoder = options
        .read_info(std::io::Cursor::new(&result.attachments[0].bytes))
        .expect("decode delivered native GIF");
    assert_eq!(
        (decoder.width(), decoder.height()),
        (BLAME_LUKE_WIDTH, BLAME_LUKE_HEIGHT)
    );
    let mut frames = 0;
    let mut last_duration = 0;
    while let Some(frame) = decoder.read_next_frame().expect("read delivered GIF frame") {
        frames += 1;
        last_duration = u32::from(frame.delay) * 10;
    }
    assert_eq!(frames, BLAME_LUKE_FRAME_COUNT);
    assert_eq!(last_duration, BLAME_LUKE_FINAL_HOLD_MS);
}

#[tokio::test]
async fn test_button_rejects_insufficient_funds_without_rendering() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 7);
    let renderer = Arc::new(FixedRenderer::bytes(b"unused"));
    let provider = provider_with(&fixture, 2, renderer.clone());
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .component_handler(BLAME_LUKE_COMPONENT_ID)
        .expect("component handler")
        .handle(component_request(USER, "Poor Clicker"), responder.clone())
        .await
        .expect("insufficient response");

    let immediate = responder.immediate.lock().expect("immediate");
    assert_eq!(immediate[0].content, INSUFFICIENT);
    assert!(immediate[0].ephemeral);
    assert!(!immediate[0].content.contains("JC"));
    assert!(renderer.calls.lock().expect("render calls").is_empty());
    assert_eq!(fixture.balance(USER), 7);
    assert!(fixture.ledger().is_empty());
}

#[tokio::test]
async fn test_button_tells_unregistered_clicker_to_register() {
    let fixture = Fixture::migrated();
    let provider = provider_with(&fixture, 0, Arc::new(FixedRenderer::bytes(b"unused")));
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .component_handler(BLAME_LUKE_COMPONENT_ID)
        .expect("component handler")
        .handle(component_request(USER, "New Clicker"), responder.clone())
        .await
        .expect("unregistered response");

    let immediate = responder.immediate.lock().expect("immediate");
    assert_eq!(immediate[0].content, UNREGISTERED);
    assert!(immediate[0].ephemeral);
}

#[tokio::test]
async fn wallet_error_says_not_charged_and_never_defers_or_renders() {
    let renderer = Arc::new(FixedRenderer::bytes(b"unused"));
    let provider = BlameLukeRegistrationProvider::with_ports(
        Arc::new(ErrorWallet),
        Arc::new(FixedSelector(1)),
        renderer.clone(),
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .component_handler(BLAME_LUKE_COMPONENT_ID)
        .expect("component handler")
        .handle(component_request(USER, "Clicker"), responder.clone())
        .await
        .expect("wallet error response");

    assert_eq!(
        responder.immediate.lock().expect("immediate")[0].content,
        WALLET_ERROR
    );
    assert!(responder.thinking_defers.lock().expect("defers").is_empty());
    assert!(renderer.calls.lock().expect("render calls").is_empty());
}

#[tokio::test]
async fn defer_failure_refunds_clicker_on_migrated_sqlite() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    let provider = provider_with(&fixture, 1, Arc::new(FixedRenderer::bytes(b"unused")));
    let responder = Arc::new(CapturingResponder::failing_defer());
    registry(&provider)
        .component_handler(BLAME_LUKE_COMPONENT_ID)
        .expect("component handler")
        .handle(component_request(USER, "Clicker"), responder.clone())
        .await
        .expect("defer failure is absorbed");

    assert_eq!(fixture.balance(USER), 100);
    assert_eq!(
        fixture.ledger(),
        [
            (-10, "blame_luke".to_owned(), Some("1".to_owned())),
            (10, "blame_luke_refund".to_owned(), None),
        ]
    );
    assert!(responder.followups.lock().expect("followups").is_empty());
}

#[tokio::test]
async fn test_render_failure_refunds_clicker() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    let provider = provider_with(&fixture, 4, Arc::new(FixedRenderer::error("render broke")));
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .component_handler(BLAME_LUKE_COMPONENT_ID)
        .expect("component handler")
        .handle(component_request(USER, "Clicker"), responder.clone())
        .await
        .expect("render failure response");

    assert_eq!(fixture.balance(USER), 100);
    assert_eq!(
        fixture.ledger(),
        [
            (-10, "blame_luke".to_owned(), Some("4".to_owned())),
            (10, "blame_luke_refund".to_owned(), None),
        ]
    );
    let followups = responder.followups.lock().expect("followups");
    assert_eq!(followups[0].content, RENDER_ERROR);
    assert!(!followups[0].content.contains("JC"));
}

#[tokio::test]
async fn delivery_failure_refunds_clicker_after_successful_render() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    let provider = provider_with(
        &fixture,
        5,
        Arc::new(FixedRenderer::bytes(b"GIF89a fixture")),
    );
    let responder = Arc::new(CapturingResponder::failing_followup());
    registry(&provider)
        .component_handler(BLAME_LUKE_COMPONENT_ID)
        .expect("component handler")
        .handle(component_request(USER, "Clicker"), responder)
        .await
        .expect("delivery failure is absorbed");

    assert_eq!(fixture.balance(USER), 100);
    assert_eq!(
        fixture.ledger(),
        [
            (-10, "blame_luke".to_owned(), Some("5".to_owned())),
            (10, "blame_luke_refund".to_owned(), None),
        ]
    );
}

#[tokio::test]
async fn guild_guard_is_ephemeral_and_precedes_any_wallet_access() {
    let provider = BlameLukeRegistrationProvider::with_ports(
        Arc::new(ErrorWallet),
        Arc::new(FixedSelector(0)),
        Arc::new(FixedRenderer::bytes(b"unused")),
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("blameluke")
        .expect("command handler")
        .handle(command_request(None), responder.clone())
        .await
        .expect("DM guard");
    let immediate = responder.immediate.lock().expect("immediate");
    assert_eq!(immediate[0].content, GUILD_ONLY_MESSAGE);
    assert!(immediate[0].ephemeral);
}
