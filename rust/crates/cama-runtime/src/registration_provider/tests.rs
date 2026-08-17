use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex as StdMutex;
use std::thread;

use cama_app::opendota_http::OpenDotaHttpConfig;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessageReceipt, DiscordMessageSnapshot,
    DiscordPresence, DiscordUserSnapshot,
};
use crate::gateway_events::{GatewayMember, GuildMemberPageSource, ReadyRecoveryContext};
use crate::registration::{InteractionAllowedMentions, InteractionResponseError, Registry};
use crate::test_support::initialize_test_database as initialize_or_migrate;

#[derive(Default)]
struct CapturedResponses {
    immediate: Vec<InteractionResponse>,
    deferred: Vec<bool>,
    followups: Vec<InteractionResponse>,
    modals: Vec<InteractionModal>,
}

#[derive(Default)]
struct CapturingResponder {
    captured: StdMutex<CapturedResponses>,
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .immediate
            .push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .deferred
            .push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .followups
            .push(response);
        Ok(())
    }

    async fn show_modal(&self, modal: InteractionModal) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .modals
            .push(modal);
        Ok(())
    }
}

#[derive(Default)]
struct MockDiscordState {
    next_message: u64,
    direct_messages: Vec<(u64, DiscordMessage)>,
    direct_edits: Vec<(DiscordMessageReceipt, DiscordMessage)>,
    channel_messages: Vec<(u64, DiscordMessage)>,
    channels: BTreeSet<(u64, u64)>,
    members: BTreeMap<(u64, u64), DiscordGuildMemberSnapshot>,
    reaction_users: Vec<DiscordUserSnapshot>,
    direct_error: Option<crate::discord_transport::DiscordDirectMessageErrorKind>,
}

#[derive(Default)]
struct MockDiscord {
    state: StdMutex<MockDiscordState>,
}

impl MockDiscord {
    fn direct_messages(&self) -> Vec<(u64, String)> {
        self.state
            .lock()
            .expect("Discord state")
            .direct_messages
            .iter()
            .map(|(user, message)| (*user, message.response.content.clone()))
            .collect()
    }

    fn direct_edits(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("Discord state")
            .direct_edits
            .iter()
            .map(|(_, message)| message.response.content.clone())
            .collect()
    }

    fn set_member(&self, guild_id: u64, user_id: u64, name: &str) {
        self.state.lock().expect("Discord state").members.insert(
            (guild_id, user_id),
            DiscordGuildMemberSnapshot {
                user_id,
                display_name: name.to_owned(),
                presence: DiscordPresence::Online,
                in_voice: false,
                deafened: false,
                activities: Vec::new(),
            },
        );
    }

    fn set_direct_error(&self, kind: crate::discord_transport::DiscordDirectMessageErrorKind) {
        self.state.lock().expect("Discord state").direct_error = Some(kind);
    }

    fn set_channel(&self, guild_id: u64, channel_id: u64) {
        self.state
            .lock()
            .expect("Discord state")
            .channels
            .insert((guild_id, channel_id));
    }
}

#[async_trait]
impl DiscordTransport for MockDiscord {
    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        Ok(None)
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        let mut state = self.state.lock().expect("Discord state");
        state.next_message += 1;
        let receipt = DiscordMessageReceipt {
            channel_id,
            message_id: state.next_message,
            jump_url: format!(
                "https://discord.com/channels/42/{channel_id}/{}",
                state.next_message
            ),
        };
        state.channel_messages.push((channel_id, message));
        Ok(receipt)
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
        Ok(900)
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
        _emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
        _user_id: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn clear_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
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
        self.state
            .lock()
            .expect("Discord state")
            .direct_messages
            .push((user_id, message));
        Ok(())
    }

    async fn send_direct_message_with_receipt(
        &self,
        user_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, crate::discord_transport::DiscordDirectMessageError> {
        let mut state = self.state.lock().expect("Discord state");
        if let Some(kind) = state.direct_error {
            return Err(crate::discord_transport::DiscordDirectMessageError {
                kind,
                message: "mock DM failure".to_owned(),
            });
        }
        state.next_message += 1;
        let receipt = DiscordMessageReceipt {
            channel_id: user_id + 10_000,
            message_id: state.next_message,
            jump_url: String::new(),
        };
        state.direct_messages.push((user_id, message));
        Ok(receipt)
    }

    async fn edit_direct_message(
        &self,
        receipt: &DiscordMessageReceipt,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.state
            .lock()
            .expect("Discord state")
            .direct_edits
            .push((receipt.clone(), message));
        Ok(())
    }

    async fn reaction_users(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
    ) -> Result<Vec<DiscordUserSnapshot>, String> {
        Ok(self
            .state
            .lock()
            .expect("Discord state")
            .reaction_users
            .clone())
    }

    async fn bot_user_id(&self) -> Result<Option<u64>, String> {
        Ok(Some(999))
    }

    async fn guild_channel_exists(&self, guild_id: u64, channel_id: u64) -> Result<bool, String> {
        Ok(self
            .state
            .lock()
            .expect("Discord state")
            .channels
            .contains(&(guild_id, channel_id)))
    }

    async fn guild_member(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        Ok(self
            .state
            .lock()
            .expect("Discord state")
            .members
            .get(&(guild_id, user_id))
            .cloned())
    }
}

#[derive(Clone, Debug)]
struct RouteServer {
    base_url: String,
    requests: Arc<StdMutex<Vec<String>>>,
}

impl RouteServer {
    fn start(bodies: Vec<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback OpenDota");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("loopback address")
        );
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let captured = requests.clone();
        thread::spawn(move || {
            for body in bodies {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let Some(request) = read_request(&mut stream) else {
                    return;
                };
                captured.lock().expect("request capture").push(request);
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(wire.as_bytes());
            }
        });
        Self { base_url, requests }
    }

    fn requests(&self, expected: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let requests = self.requests.lock().expect("request capture").clone();
            if requests.len() >= expected || Instant::now() >= deadline {
                return requests;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1_024];
    while bytes.len() < 16 * 1_024 {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes)
        .ok()?
        .lines()
        .next()
        .map(ToOwned::to_owned)
}

fn database() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary registration database");
    initialize_or_migrate(database.path()).expect("Rust schema migration");
    database
}

fn services(server: &RouteServer) -> Arc<OpenDotaRuntimeServices> {
    let mut config = OpenDotaHttpConfig::new(&server.base_url, None);
    config.request_timeout = Duration::from_secs(2);
    config.request_deadline = Duration::from_secs(5);
    config.retry_delays = vec![Duration::ZERO];
    config.requests_per_minute = 1_200;
    Arc::new(OpenDotaRuntimeServices::with_config(config).expect("loopback OpenDota services"))
}

fn config() -> PlayerRegistrationConfig {
    PlayerRegistrationConfig {
        mmr_modal_retry_limit: 3,
        mmr_modal_timeout: Duration::from_secs(300),
        new_player_exclusion_boost: 4,
        neon_degen_enabled: false,
        lobby_rally_cooldown: Duration::from_secs(120),
        lobby_ready_cooldown: Duration::from_secs(60),
        lobby_channel_id: None,
        low_skill_lobby_channel_id: None,
        rating_system: cama_domain::rating::CamaRatingSystem::default(),
        openskill: cama_domain::openskill::CamaOpenSkillSystem::default(),
    }
}

fn registry(provider: &PlayerRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register provider");
    builder.build()
}

fn player_command(
    interaction_id: u64,
    user_id: u64,
    subcommand: &str,
    options: Vec<InteractionOption>,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id,
        name: "player".to_owned(),
        user_id,
        user_display_name: format!("Player {user_id}"),
        guild_id: Some(42),
        channel_id: Some(9),
        member_permissions: None,
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(options),
        }],
    }
}

fn option(name: &str, value: InteractionValue) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value,
    }
}

async fn region_response(
    preferred_region: Option<&str>,
    inferred_region: Option<&str>,
    selected_region: Option<&str>,
    registered: bool,
) -> InteractionResponse {
    let database = database();
    if registered {
        let connection = Connection::open(database.path()).expect("open registration database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production foreign-key mode");
        connection
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,preferred_region,inferred_region
                 ) VALUES(100,42,'Region Player',?1,?2)",
                params![preferred_region, inferred_region],
            )
            .expect("seed region player");
    }
    let server = RouteServer::start(Vec::new());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let registry = registry(&provider);
    let responder = Arc::new(CapturingResponder::default());
    let options = selected_region.map_or_else(Vec::new, |region| {
        vec![option(
            "region",
            InteractionValue::String(region.to_owned()),
        )]
    });
    registry
        .command_handler("player")
        .expect("player handler")
        .handle(
            player_command(500, 100, "region", options),
            responder.clone(),
        )
        .await
        .expect("region response");
    responder
        .captured
        .lock()
        .expect("response capture")
        .followups
        .pop()
        .expect("region followup")
}

struct UnusedMembers;

#[async_trait]
impl GuildMemberPageSource for UnusedMembers {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        panic!("region backfill must not fetch Discord members")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ready_region_backfill_dedupes_steam_ids_and_ignores_foreign_guilds() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open registration database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("production foreign-key mode");
    for (discord_id, guild_id, steam_id) in
        [(101, 42, 99), (101, 43, 99), (102, 42, 77), (103, 43, 88)]
    {
        connection
            .execute(
                "INSERT INTO players (
                     discord_id,guild_id,discord_username,steam_id,inferred_region
                 ) VALUES (?1,?2,'region-backfill',?3,NULL)",
                params![discord_id, guild_id, steam_id],
            )
            .expect("insert backfill player");
    }
    let server = RouteServer::start(vec![
        r#"{"region":{"1":{"games":7}}}"#,
        r#"{"region":{"2":{"games":4}}}"#,
    ]);
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let report = provider
        .region_backfill_observer()
        .ready_recovery(ReadyRecoveryContext::new(vec![42], Arc::new(UnusedMembers)))
        .await;
    assert!(report.failures.is_empty());
    assert_eq!(report.guilds_attempted, 1);
    assert_eq!(report.members_refreshed, 2);
    let requests = server.requests(2);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /players/99/counts "))
            .count(),
        1
    );
    assert!(
        requests
            .iter()
            .all(|request| !request.starts_with("GET /players/88/counts ")),
        "foreign-guild Steam IDs must not trigger OpenDota HTTP"
    );
    let regions = connection
        .prepare(
            "SELECT discord_id,guild_id,inferred_region FROM players
             WHERE discord_id IN (101,102,103) ORDER BY discord_id,guild_id",
        )
        .expect("prepare region read")
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .expect("read regions")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect regions");
    assert_eq!(
        regions,
        [
            (101, 42, Some("USW".to_owned())),
            (101, 43, None),
            (102, 42, Some("USE".to_owned())),
            (103, 43, None),
        ]
    );
}

#[tokio::test]
async fn test_writes_sentinel_when_no_us_games() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open registration database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production foreign-key mode");
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username,inferred_region)
             VALUES(100,42,'No US Games',NULL)",
            [],
        )
        .expect("seed player");
    let server = RouteServer::start(vec![r#"{"region":{"3":{"games":200}}}"#]);
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    provider
        .handler
        .best_effort_infer_region(
            &CommandContext {
                interaction_id: 1,
                name: "player".to_owned(),
                user_id: 100,
                user_display_name: "No US Games".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                options: Vec::new(),
            },
            42,
            99,
        )
        .await;
    assert_eq!(
        connection
            .query_row(
                "SELECT inferred_region FROM players WHERE discord_id=100 AND guild_id=42",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("read inferred region"),
        "NONE"
    );
}

#[tokio::test]
async fn test_registration_no_write_when_counts_unavailable() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open registration database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production foreign-key mode");
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username,inferred_region)
             VALUES(100,42,'Unavailable Counts',NULL)",
            [],
        )
        .expect("seed player");
    let server = RouteServer::start(Vec::new());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    provider
        .handler
        .best_effort_infer_region(
            &CommandContext {
                interaction_id: 1,
                name: "player".to_owned(),
                user_id: 100,
                user_display_name: "Unavailable Counts".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                options: Vec::new(),
            },
            42,
            99,
        )
        .await;
    assert!(
        connection
            .query_row(
                "SELECT inferred_region FROM players WHERE discord_id=100 AND guild_id=42",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .expect("read inferred region")
            .is_none()
    );
}

#[tokio::test]
async fn test_backfill_skips_rows_when_counts_unavailable() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open registration database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production foreign-key mode");
    connection
        .execute(
            "INSERT INTO players(
                 discord_id,guild_id,discord_username,steam_id,inferred_region
             ) VALUES(100,42,'Unavailable Backfill',99,NULL)",
            [],
        )
        .expect("seed player");
    let server = RouteServer::start(Vec::new());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let report = provider
        .region_backfill_observer()
        .ready_recovery(ReadyRecoveryContext::new(vec![42], Arc::new(UnusedMembers)))
        .await;
    assert!(report.failures.is_empty());
    assert_eq!(report.members_refreshed, 0);
    assert!(
        connection
            .query_row(
                "SELECT inferred_region FROM players WHERE discord_id=100 AND guild_id=42",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .expect("read inferred region")
            .is_none()
    );
}

#[tokio::test]
async fn test_valid_pick_persists() {
    let response = region_response(None, None, Some("USW"), true).await;
    assert_eq!(response.content, "✅ Set your server to **US West**.");
    assert!(response.ephemeral);
}

#[tokio::test]
async fn test_invalid_code_rejected_before_write() {
    let response = region_response(None, None, Some("EU"), true).await;
    assert_eq!(response.content, "❌ Invalid region");
    assert!(response.ephemeral);
}

#[tokio::test]
async fn test_unregistered_player_rejected() {
    let response = region_response(None, None, Some("USE"), false).await;
    assert_eq!(response.content, "❌ Player not registered.");
    assert!(response.ephemeral);
}

#[tokio::test]
async fn test_explicit_pick_reports_set() {
    let response = region_response(Some("USE"), Some("USW"), None, true).await;
    assert!(response.content.contains("server is set to **US East**"));
    assert!(!response.content.contains("inferred"));
}

#[tokio::test]
async fn test_inferred_only_reports_inferred() {
    let response = region_response(None, Some("USW"), None, true).await;
    assert!(response.content.contains("server is **US West**"));
    assert!(response.content.contains("inferred from your Dota history"));
}

#[tokio::test]
async fn test_sentinel_reports_none() {
    let response = region_response(None, Some("NONE"), None, true).await;
    assert!(response.content.contains("haven't set a server yet"));
    assert!(!response.content.contains("inferred from your Dota history"));
}

#[test]
fn command_schema_matches_python_extension() {
    let database = database();
    let server = RouteServer::start(Vec::new());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let schema_registry = registry(&provider);
    let names = schema_registry
        .commands()
        .map(|command| command.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(names, BTreeSet::from(["player", "refer"]));
    let refer = schema_registry
        .commands()
        .find(|command| command.name == "refer")
        .expect("refer schema");
    assert_eq!(refer.description, "Refer a player before their first game");
    assert_eq!(refer.options.len(), 1);
    assert_eq!(refer.options[0].name, "player");
    assert_eq!(refer.options[0].description, "…");
    assert_eq!(refer.options[0].kind, CommandOptionKind::User);
    assert!(refer.options[0].required);
    let player = schema_registry
        .commands()
        .find(|command| command.name == "player")
        .expect("player schema");
    assert_eq!(player.options.len(), 11);
    assert_eq!(
        player
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        [
            "lobby",
            "curfew",
            "timezone",
            "playtime",
            "register",
            "link",
            "unlink",
            "steamids",
            "roles",
            "region",
            "exclusion"
        ]
    );
    let register = &player.options[4];
    assert_eq!(register.description, "Register yourself as a player");
    assert_eq!(register.options[0].name, "steam_id");
    assert_eq!(register.options[0].kind, CommandOptionKind::Integer);
    assert!(register.options[0].required);
    let link = &player.options[5];
    assert_eq!(
        link.options
            .iter()
            .map(|option| (option.name.as_str(), option.kind, option.required))
            .collect::<Vec<_>>(),
        [
            ("steam_id", CommandOptionKind::Integer, true),
            ("set_primary", CommandOptionKind::Boolean, false)
        ]
    );
    let region = &player.options[9].options[0];
    assert_eq!(
        region.choices,
        [
            CommandOptionChoice::String {
                name: "US East".to_owned(),
                value: "USE".to_owned()
            },
            CommandOptionChoice::String {
                name: "US West".to_owned(),
                value: "USW".to_owned()
            }
        ]
    );
    let lobby = &player.options[0];
    assert_eq!(lobby.kind, CommandOptionKind::SubcommandGroup);
    assert_eq!(lobby.description, "Lobby notification preferences");
    assert_eq!(
        lobby
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        ["autonotify", "status"]
    );
    assert_eq!(
        lobby.options[0]
            .options
            .iter()
            .map(|option| (option.name.as_str(), option.kind, option.required))
            .collect::<Vec<_>>(),
        [
            ("enabled", CommandOptionKind::Boolean, false),
            ("playername", CommandOptionKind::User, false)
        ]
    );
    let curfew = &player.options[1];
    assert_eq!(curfew.kind, CommandOptionKind::SubcommandGroup);
    assert_eq!(
        curfew
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        ["add", "remove", "list"]
    );
    assert_eq!(
        curfew.options[0]
            .options
            .iter()
            .map(|option| (option.name.as_str(), option.kind, option.required))
            .collect::<Vec<_>>(),
        [
            ("name", CommandOptionKind::String, true),
            ("start", CommandOptionKind::String, true),
            ("end", CommandOptionKind::String, true),
            ("timezone", CommandOptionKind::String, false),
            ("days", CommandOptionKind::String, false)
        ]
    );
    assert_eq!(
        curfew.options[1]
            .options
            .iter()
            .map(|option| (option.name.as_str(), option.kind, option.required))
            .collect::<Vec<_>>(),
        [("name", CommandOptionKind::String, true)]
    );
    let timezone = &player.options[2];
    assert_eq!(timezone.kind, CommandOptionKind::SubcommandGroup);
    assert_eq!(
        timezone
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        ["set", "status", "list"]
    );
    assert_eq!(
        timezone.options[0]
            .options
            .iter()
            .map(|option| (option.name.as_str(), option.kind, option.required))
            .collect::<Vec<_>>(),
        [("timezone", CommandOptionKind::String, true)]
    );
    let playtime = &player.options[3];
    assert_eq!(playtime.kind, CommandOptionKind::SubcommandGroup);
    assert_eq!(
        playtime
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        ["set", "clear", "status", "popular"]
    );
    assert_eq!(
        playtime.options[0]
            .options
            .iter()
            .map(|option| (option.name.as_str(), option.kind, option.required))
            .collect::<Vec<_>>(),
        [("hours", CommandOptionKind::String, true)]
    );
    assert_eq!(
        schema_registry.component_routes()[0].custom_id_prefix,
        MMR_COMPONENT_PREFIX
    );
}

#[tokio::test]
async fn test_referral_uses_private_ack_before_public_onboarding() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open referral database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy foreign keys for referral fixture");
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username)
             VALUES(10,42,'Referrer')",
            [],
        )
        .expect("seed referral referrer");
    let server = RouteServer::start(Vec::new());
    let discord = Arc::new(MockDiscord::default());
    discord.set_channel(42, 111);
    discord.set_channel(42, 222);
    let mut registration_config = config();
    registration_config.lobby_channel_id = Some(111);
    registration_config.low_skill_lobby_channel_id = Some(222);
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord,
        registration_config,
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("refer")
        .expect("refer handler")
        .handle(
            InteractionRequest::Command {
                interaction_id: 700,
                name: "refer".to_owned(),
                user_id: 10,
                user_display_name: "Referrer".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                options: vec![InteractionOption {
                    name: "player".to_owned(),
                    value: InteractionValue::User {
                        id: 20,
                        display_name: Some("New player".to_owned()),
                        is_bot: Some(false),
                    },
                }],
            },
            responder.clone(),
        )
        .await
        .expect("refer response");
    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.deferred, [true]);
    assert_eq!(captured.followups.len(), 2);
    assert_eq!(captured.followups[0].content, "✅ Referral created.");
    assert!(captured.followups[0].ephemeral);
    assert_eq!(
        captured.followups[1].content,
        concat!(
            "**Referral: <@20>**\n",
            "1. If needed, run `/player register` with your Steam32 ID ",
            "(the number in your Dotabuff URL).\n",
            "2. Run `/player roles` with your Dota positions (1-5).\n",
            "3. React in <#111> or <#222> to join an active lobby, ",
            "or run `/lobby` to start one."
        )
    );
    assert!(!captured.followups[1].ephemeral);
}

#[tokio::test]
async fn test_referral_onboarding_falls_back_when_lobby_channels_do_not_resolve() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open referral database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy foreign keys for referral fixture");
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username)
             VALUES(10,42,'Referrer')",
            [],
        )
        .expect("seed referral referrer");
    let server = RouteServer::start(Vec::new());
    let discord = Arc::new(MockDiscord::default());
    let mut registration_config = config();
    registration_config.lobby_channel_id = Some(111);
    registration_config.low_skill_lobby_channel_id = Some(222);
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord,
        registration_config,
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("refer")
        .expect("refer handler")
        .handle(
            InteractionRequest::Command {
                interaction_id: 701,
                name: "refer".to_owned(),
                user_id: 10,
                user_display_name: "Referrer".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                options: vec![InteractionOption {
                    name: "player".to_owned(),
                    value: InteractionValue::User {
                        id: 20,
                        display_name: Some("New player".to_owned()),
                        is_bot: Some(false),
                    },
                }],
            },
            responder.clone(),
        )
        .await
        .expect("refer response");
    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.followups.len(), 2);
    assert!(captured.followups[1].content.ends_with(
        "3. React in a lobby channel to join an active lobby, or run `/lobby` to start one."
    ));
    assert!(!captured.followups[1].ephemeral);
}

#[test]
fn test_player_lobby_status_has_no_target_player_option() {
    let database = database();
    let server = RouteServer::start(Vec::new());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let schema_registry = registry(&provider);
    let player = schema_registry
        .commands()
        .find(|command| command.name == "player")
        .expect("player schema");
    let lobby = player
        .options
        .iter()
        .find(|option| option.name == "lobby")
        .expect("lobby group");
    let status = lobby
        .options
        .iter()
        .find(|option| option.name == "status")
        .expect("lobby status command");
    assert!(status.options.is_empty());
}

async fn lobby_status_response(database: &NamedTempFile, user_id: u64) -> InteractionResponse {
    let server = RouteServer::start(Vec::new());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("player")
        .expect("player handler")
        .handle(
            InteractionRequest::Command {
                interaction_id: 600,
                name: "player".to_owned(),
                user_id,
                user_display_name: format!("Player {user_id}"),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                options: vec![InteractionOption {
                    name: "lobby".to_owned(),
                    value: InteractionValue::SubcommandGroup(vec![InteractionOption {
                        name: "status".to_owned(),
                        value: InteractionValue::Subcommand(Vec::new()),
                    }]),
                }],
            },
            responder.clone(),
        )
        .await
        .expect("lobby status response");
    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.deferred, [true]);
    captured
        .followups
        .last()
        .expect("lobby status followup")
        .clone()
}

#[tokio::test]
async fn test_player_lobby_status_exposes_only_the_callers_suspension() {
    let database = database();
    let now = now_seconds();
    Connection::open(database.path())
        .expect("open lobby status database")
        .execute_batch(&format!(
            "INSERT INTO lobby_suspensions(
                 guild_id,discord_id,scope,completion,expires_at,
                 matches_required,matches_remaining,pending_match_watermark,
                 reason,source,actor_id,revision,active,created_at,updated_at
             ) VALUES(42,42,'open','both',{},4,2,0,
                      'repeat abandonment','admin',999,1,1,{now},{now});
             INSERT INTO low_priority_state(
                 discord_id,guild_id,wins_required,wins_remaining,active,reason,set_by
             ) VALUES(42,42,7,3,1,'missed ready checks',999);",
            now + 9_000
        ))
        .expect("seed private lobby status");
    let response = lobby_status_response(&database, 42).await;
    assert!(response.ephemeral);
    assert!(response.content.contains("Lobby suspension"));
    assert!(response.content.contains("All You Can Feed"));
    assert!(response.content.contains(&format!("<t:{}:F>", now + 9_000)));
    assert!(
        response
            .content
            .contains("and 2 completed matches remaining")
    );
    assert!(response.content.contains("Reason: repeat abandonment"));
    assert!(
        !response
            .content
            .to_ascii_lowercase()
            .contains("low priority")
    );
    assert!(!response.content.to_ascii_lowercase().contains("progress"));
    assert!(!response.content.contains("missed ready checks"));
    assert!(!response.content.contains("<@42>"));
}

#[tokio::test]
async fn test_player_lobby_status_reports_no_active_suspension() {
    let database = database();
    let response = lobby_status_response(&database, 88).await;
    assert!(response.ephemeral);
    assert_eq!(
        response.content,
        "✅ All clear — you have no active lobby suspension."
    );
}

#[test]
fn neon_registration_one_time_event_is_restart_safe_in_migrated_sqlite() {
    let database = database();
    let port: Arc<dyn NeonEventPort> = Arc::new(SqliteNeonEventPort {
        repository: NeonEventRepository::new(database.path()),
    });
    let mut first = NeonDegenService::with_event_port(1, port.clone());
    first.queue_rolls([0.0]);
    assert!(
        first
            .on_registration(901, Some(42), "Neon Player")
            .and_then(|result| result.text_block)
            .is_some()
    );
    let mut restarted = NeonDegenService::with_event_port(2, port);
    restarted.queue_rolls([0.0]);
    assert!(
        restarted
            .on_registration(901, Some(42), "Neon Player")
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_registration_uses_one_loopback_fetch_and_atomic_migrated_sqlite() {
    let database = database();
    let server = RouteServer::start(vec![
        r#"{"profile":{"personaname":"Dota Name"},"mmr_estimate":{"estimate":5000}}"#,
        r#"{"region":{"2":{"games":50},"1":{"games":10}}}"#,
    ]);
    let discord = Arc::new(MockDiscord::default());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord,
        config(),
    );
    let production_registry = registry(&provider);
    let responder = Arc::new(CapturingResponder::default());
    production_registry
        .command_handler("player")
        .expect("player handler")
        .handle(
            player_command(
                7,
                100,
                "register",
                vec![option("steam_id", InteractionValue::Integer(12_345))],
            ),
            responder.clone(),
        )
        .await
        .expect("registration response");
    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.deferred, [true]);
    let success = captured.followups.last().expect("success followup");
    assert!(!success.ephemeral);
    assert!(success.content.starts_with("✅ Registered <@100>!"));
    assert_eq!(
        success.allowed_mentions,
        InteractionAllowedMentions::Users(vec![100])
    );
    drop(captured);
    let requests = server.requests(2);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /players/12345 "))
            .count(),
        1,
        "MMR fallback must reuse its single player payload"
    );
    assert!(requests[1].starts_with("GET /players/12345/counts "));
    let connection = Connection::open(database.path()).expect("open registration database");
    let player: (i64, i64, String) = connection
        .query_row(
            "SELECT initial_mmr,exclusion_count,inferred_region FROM players WHERE discord_id=100 AND guild_id=42",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("registered player");
    assert_eq!(player, (5_000, 4, "USE".to_owned()));
    let primary: (i64, bool) = connection
        .query_row(
            "SELECT steam_id,is_primary FROM player_steam_ids WHERE discord_id=100",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("primary Steam link");
    assert_eq!(primary, (12_345, true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_register_with_opendota_failure() {
    let database = database();
    let server = RouteServer::start(vec!["null"]);
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("player")
        .expect("player handler")
        .handle(
            player_command(
                8,
                99_999,
                "register",
                vec![option("steam_id", InteractionValue::Integer(12_345_678))],
            ),
            responder.clone(),
        )
        .await
        .expect("friendly OpenDota failure response");

    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.deferred, [true]);
    let failure = captured.followups.last().expect("failure followup");
    assert!(failure.ephemeral);
    assert!(
        failure
            .content
            .contains("Could not fetch player data from OpenDota")
    );
    drop(captured);
    assert_eq!(server.requests(1).len(), 1);
    let connection = Connection::open(database.path()).expect("inspect failed registration");
    let persisted: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=99999 AND guild_id=42)",
            [],
            |row| row.get(0),
        )
        .expect("check player rollback");
    let linked: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM player_steam_ids WHERE steam_id=12345678)",
            [],
            |row| row.get(0),
        )
        .expect("check Steam rollback");
    assert!(!persisted && !linked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_register_with_valid_opendota_response() {
    let database = database();
    let server = RouteServer::start(vec![
        r#"{"profile":{"personaname":"Dota Name"},"rank_tier":50,"mmr_estimate":{"estimate":2000}}"#,
        "{}",
    ]);
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("player")
        .expect("player handler")
        .handle(
            player_command(
                9,
                100_001,
                "register",
                vec![option("steam_id", InteractionValue::Integer(87_654_321))],
            ),
            responder.clone(),
        )
        .await
        .expect("successful registration response");

    let captured = responder.captured.lock().expect("response capture");
    let success = captured.followups.last().expect("success followup");
    assert!(!success.ephemeral);
    assert!(success.content.starts_with("✅ Registered <@100001>!"));
    drop(captured);
    assert_eq!(server.requests(2).len(), 2);
    let connection = Connection::open(database.path()).expect("inspect valid registration");
    let player: (i64, f64) = connection
        .query_row(
            "SELECT initial_mmr,glicko_rating FROM players
             WHERE discord_id=100001 AND guild_id=42",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("registered player");
    let expected =
        cama_domain::rating::CamaRatingSystem::default().create_player_from_mmr(Some(2_000));
    assert_eq!(player.0, 2_000);
    assert!((player.1 - expected.rating).abs() < 1.0e-12);
    let primary: (i64, bool) = connection
        .query_row(
            "SELECT steam_id,is_primary FROM player_steam_ids WHERE discord_id=100001",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("primary Steam link");
    assert_eq!(primary, (87_654_321, true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn computed_mmr_float_registers_without_manual_mmr_prompt() {
    let database = database();
    let server = RouteServer::start(vec![
        r#"{"profile":{"personaname":"PMA arc"},"computed_mmr":3811.28}"#,
        "{}",
    ]);
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let responder = Arc::new(CapturingResponder::default());

    registry(&provider)
        .command_handler("player")
        .expect("player handler")
        .handle(
            player_command(
                10,
                100_002,
                "register",
                vec![option("steam_id", InteractionValue::Integer(11_758_567))],
            ),
            responder.clone(),
        )
        .await
        .expect("successful registration response");

    let captured = responder.captured.lock().expect("response capture");
    let success = captured.followups.last().expect("success followup");
    assert!(success.content.starts_with("✅ Registered <@100002>!"));
    assert!(success.components.is_empty(), "manual MMR prompt was shown");
    drop(captured);
    assert_eq!(
        server
            .requests(2)
            .iter()
            .filter(|request| request.starts_with("GET /players/11758567 "))
            .count(),
        1,
        "registration must reuse the one OpenDota player payload"
    );
    let connection = Connection::open(database.path()).expect("inspect registration");
    assert_eq!(
        connection
            .query_row(
                "SELECT initial_mmr FROM players WHERE discord_id=100002 AND guild_id=42",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("registered player"),
        3_811
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mmr_button_and_modal_survive_provider_restart_without_refetch() {
    let database = database();
    let server = RouteServer::start(vec![r#"{"mmr_estimate":{"estimate":null}}"#]);
    let discord = Arc::new(MockDiscord::default());
    discord.set_member(42, 101, "Restarted Player");
    let first = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord.clone(),
        config(),
    );
    let first_registry = registry(&first);
    let prompt = Arc::new(CapturingResponder::default());
    first_registry
        .command_handler("player")
        .expect("player handler")
        .handle(
            player_command(
                88,
                101,
                "register",
                vec![option("steam_id", InteractionValue::Integer(54_321))],
            ),
            prompt.clone(),
        )
        .await
        .expect("MMR prompt");
    let custom_id = prompt.captured.lock().expect("prompt capture").followups[0].components[0]
        .buttons[0]
        .custom_id
        .clone();

    let restarted = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord,
        config(),
    );
    let restarted_registry = registry(&restarted);
    let button = Arc::new(CapturingResponder::default());
    restarted_registry
        .component_handler(&custom_id)
        .expect("restart-safe button route")
        .handle(
            InteractionRequest::Component {
                interaction_id: 89,
                custom_id,
                user_id: 101,
                user_display_name: "Registration Player 101".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                values: Vec::new(),
            },
            button.clone(),
        )
        .await
        .expect("show modal");
    let modal_id = button.captured.lock().expect("button capture").modals[0]
        .custom_id
        .clone();
    assert_eq!(
        button.captured.lock().expect("button capture").modals[0].title,
        "Enter MMR"
    );
    let modal = Arc::new(CapturingResponder::default());
    restarted_registry
        .component_handler(&modal_id)
        .expect("modal route")
        .handle(
            InteractionRequest::Modal {
                interaction_id: 90,
                custom_id: modal_id,
                user_id: 101,
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                fields: BTreeMap::from([("mmr".to_owned(), "4200".to_owned())]),
            },
            modal.clone(),
        )
        .await
        .expect("finish modal registration");
    let captured = modal.captured.lock().expect("modal capture");
    assert_eq!(captured.immediate[0].content, "✅ MMR received");
    assert!(captured.followups[0].content.contains("Registered <@101>"));
    drop(captured);
    assert_eq!(server.requests(1).len(), 1, "manual MMR must not refetch");
    let connection = Connection::open(database.path()).expect("open registration database");
    assert_eq!(
        connection
            .query_row(
                "SELECT initial_mmr,inferred_region
                 FROM players WHERE discord_id=101 AND guild_id=42",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .expect("modal-registered player"),
        (4_200, None),
        "the manual-MMR path must skip region inference"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_shot_autonotify_preflights_persists_and_delivers_after_restart() {
    let database = database();
    let server = RouteServer::start(Vec::new());
    let discord = Arc::new(MockDiscord::default());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord.clone(),
        config(),
    );
    let initial_registry = registry(&provider);
    let responder = Arc::new(CapturingResponder::default());
    initial_registry
        .command_handler("player")
        .expect("player handler")
        .handle(
            InteractionRequest::Command {
                interaction_id: 99,
                name: "player".to_owned(),
                user_id: 200,
                user_display_name: "Subscriber".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                options: vec![InteractionOption {
                    name: "lobby".to_owned(),
                    value: InteractionValue::SubcommandGroup(vec![InteractionOption {
                        name: "autonotify".to_owned(),
                        value: InteractionValue::Subcommand(vec![option(
                            "playername",
                            InteractionValue::User {
                                id: 201,
                                display_name: Some("Target_*".to_owned()),
                                is_bot: Some(false),
                            },
                        )]),
                    }]),
                }],
            },
            responder,
        )
        .await
        .expect("arm one-shot alert");
    assert!(discord.direct_messages()[0].1.contains("DM check passed"));
    assert!(discord.direct_edits()[0].contains("One-time lobby alert set"));

    let restarted = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord.clone(),
        config(),
    );
    restarted
        .lobby_join_observer()
        .confirmed_lobby_join(ConfirmedLobbyJoin {
            guild_id: 42,
            player_id: 201,
            player_name: "Target".to_owned(),
            player_display_name: "Target_*".to_owned(),
            lobby_kind: cama_app::embeds::LobbyKind::Open,
            joined_at_ns: i64::MAX,
            player_ids: BTreeSet::from([201]),
            ready_threshold: 10,
            lobby_channel_id: None,
            lobby_message_id: None,
            origin_channel_id: None,
            thread_id: None,
        })
        .await
        .expect("deliver persisted alert");
    let messages = discord.direct_messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].0, 200);
    assert_eq!(
        messages[1].1,
        "🔔 **Target\\_\\*** just joined 🍽️ All You Can Feed! This one-time lobby notification has now been used."
    );
    let connection = Connection::open(database.path()).expect("open notification database");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM lobby_target_subscriptions",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("subscription count"),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_dm_does_not_persist_one_shot_subscription() {
    let database = database();
    let server = RouteServer::start(Vec::new());
    let discord = Arc::new(MockDiscord::default());
    discord.set_direct_error(crate::discord_transport::DiscordDirectMessageErrorKind::Forbidden);
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord,
        config(),
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("player")
        .expect("player handler")
        .handle(
            InteractionRequest::Command {
                interaction_id: 100,
                name: "player".to_owned(),
                user_id: 210,
                user_display_name: "Subscriber".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                options: vec![InteractionOption {
                    name: "lobby".to_owned(),
                    value: InteractionValue::SubcommandGroup(vec![InteractionOption {
                        name: "autonotify".to_owned(),
                        value: InteractionValue::Subcommand(vec![option(
                            "playername",
                            InteractionValue::User {
                                id: 211,
                                display_name: Some("Target".to_owned()),
                                is_bot: Some(false),
                            },
                        )]),
                    }]),
                }],
            },
            responder.clone(),
        )
        .await
        .expect("blocked DM response");
    assert_eq!(
        responder
            .captured
            .lock()
            .expect("blocked DM response")
            .followups[0]
            .content,
        "❌ I can't DM you. Enable direct messages from server members, then try again."
    );
    assert!(
        !NotificationRepository::new(database.path())
            .has_lobby_target_subscription(210, 211, Some(42))
            .expect("subscription lookup")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_autonotify_restarts_and_combines_clipboard_rally_mentions() {
    let database = database();
    let server = RouteServer::start(Vec::new());
    let discord = Arc::new(MockDiscord::default());
    discord
        .state
        .lock()
        .expect("Discord state")
        .reaction_users
        .push(DiscordUserSnapshot {
            user_id: 501,
            display_name: "Reactor".to_owned(),
            is_bot: false,
        });
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord.clone(),
        config(),
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("player")
        .expect("player handler")
        .handle(
            InteractionRequest::Command {
                interaction_id: 101,
                name: "player".to_owned(),
                user_id: 500,
                user_display_name: "Persistent".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                options: vec![InteractionOption {
                    name: "lobby".to_owned(),
                    value: InteractionValue::SubcommandGroup(vec![InteractionOption {
                        name: "autonotify".to_owned(),
                        value: InteractionValue::Subcommand(vec![option(
                            "enabled",
                            InteractionValue::Boolean(true),
                        )]),
                    }]),
                }],
            },
            responder,
        )
        .await
        .expect("enable persistent lobby preference");

    let restarted = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord.clone(),
        config(),
    );
    restarted
        .lobby_join_observer()
        .confirmed_lobby_join(ConfirmedLobbyJoin {
            guild_id: 42,
            player_id: 308,
            player_name: "Eighth".to_owned(),
            player_display_name: "Eighth".to_owned(),
            lobby_kind: cama_app::embeds::LobbyKind::Open,
            joined_at_ns: i64::MAX,
            player_ids: (301..=308).collect(),
            ready_threshold: 10,
            lobby_channel_id: Some(70),
            lobby_message_id: Some(71),
            origin_channel_id: Some(72),
            thread_id: Some(73),
        })
        .await
        .expect("deliver lobby rally");
    let state = discord.state.lock().expect("Discord state");
    assert_eq!(state.channel_messages.len(), 2);
    assert_eq!(state.channel_messages[0].0, 72);
    assert_eq!(
        state.channel_messages[0].1.response.content,
        "<@501> <@500>"
    );
    assert_eq!(
        state.channel_messages[0].1.allowed_mentions,
        crate::discord_transport::DiscordAllowedMentions::Users(BTreeSet::from([500, 501]))
    );
    assert_eq!(
        state.channel_messages[0].1.response.embeds[0]
            .title
            .as_deref(),
        Some("📢 🍽️ All You Can Feed Almost Ready!")
    );
    assert_eq!(state.channel_messages[1].0, 73);
    assert!(
        state.channel_messages[1]
            .1
            .response
            .content
            .contains("**+2** more players")
    );
}

fn confirmed_join(
    player_id: u64,
    player_count: usize,
    ready_threshold: usize,
) -> ConfirmedLobbyJoin {
    ConfirmedLobbyJoin {
        guild_id: 42,
        player_id,
        player_name: format!("Player {player_id}"),
        player_display_name: format!("Display {player_id}"),
        lobby_kind: cama_app::embeds::LobbyKind::Open,
        joined_at_ns: i64::MAX,
        player_ids: (1..=u64::try_from(player_count).expect("player count")).collect(),
        ready_threshold,
        lobby_channel_id: Some(70),
        lobby_message_id: Some(71),
        origin_channel_id: Some(72),
        thread_id: Some(73),
    }
}

#[tokio::test]
async fn exact_threshold_sends_one_python_ready_embed_instead_of_a_rally() {
    let database = database();
    let server = RouteServer::start(Vec::new());
    let discord = Arc::new(MockDiscord::default());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord.clone(),
        config(),
    );
    let event = confirmed_join(10, 10, 10);

    provider
        .lobby_join_observer()
        .confirmed_lobby_join(event.clone())
        .await
        .expect("ready announcement");
    provider
        .lobby_join_observer()
        .confirmed_lobby_join(event)
        .await
        .expect("cooldown duplicate is ignored");

    let state = discord.state.lock().expect("Discord state");
    assert_eq!(state.channel_messages.len(), 1);
    assert_eq!(state.channel_messages[0].0, 72);
    let response = &state.channel_messages[0].1.response;
    assert_eq!(response.content, "");
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("🎮 🍽️ All You Can Feed Ready!")
    );
    assert_eq!(
        response.embeds[0].description.as_deref(),
        Some("All You Can Feed now has 10 players!")
    );
    assert!(response.embeds[0].fields.iter().any(|field| {
        field.name == "Next Step" && field.value == "Run `/readycheck` before `/shuffle`."
    }));
    assert!(
        response.embeds[0].fields.iter().any(|field| {
            field.value == "[Jump to Lobby](https://discord.com/channels/42/70/71)"
        })
    );
}

#[tokio::test]
async fn lobby_reset_clears_both_ready_and_rally_generation_cooldowns() {
    let database = database();
    let server = RouteServer::start(Vec::new());
    let discord = Arc::new(MockDiscord::default());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord.clone(),
        config(),
    );
    let observer = provider.lobby_join_observer();
    let rally = confirmed_join(8, 8, 10);
    let ready = confirmed_join(10, 10, 10);

    observer
        .confirmed_lobby_join(rally.clone())
        .await
        .expect("first rally");
    observer
        .confirmed_lobby_join(ready.clone())
        .await
        .expect("first ready");
    let first_count = discord
        .state
        .lock()
        .expect("Discord state")
        .channel_messages
        .len();
    observer
        .confirmed_lobby_join(rally.clone())
        .await
        .expect("rally cooldown");
    observer
        .confirmed_lobby_join(ready.clone())
        .await
        .expect("ready cooldown");
    assert_eq!(
        discord
            .state
            .lock()
            .expect("Discord state")
            .channel_messages
            .len(),
        first_count
    );

    observer
        .lobby_reset(42, cama_app::embeds::LobbyKind::Open)
        .await
        .expect("reset cooldowns");
    observer
        .confirmed_lobby_join(rally)
        .await
        .expect("rally after reset");
    observer
        .confirmed_lobby_join(ready)
        .await
        .expect("ready after reset");
    assert_eq!(
        discord
            .state
            .lock()
            .expect("Discord state")
            .channel_messages
            .len(),
        first_count * 2
    );
}

#[tokio::test]
async fn lobby_neon_routes_only_explicit_join_and_gamba_to_the_python_channels() {
    let database = database();
    let server = RouteServer::start(Vec::new());
    let discord = Arc::new(MockDiscord::default());
    let mut runtime_config = config();
    runtime_config.neon_degen_enabled = true;
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord.clone(),
        runtime_config,
    );
    provider
        .handler
        .neon
        .lock()
        .expect("Neon service")
        .queue_rolls([0.0]);
    let event = confirmed_join(100, 7, 10);

    provider
        .lobby_join_observer()
        .explicit_lobby_join_neon(event)
        .await
        .expect("explicit join Neon");
    provider
        .handler
        .neon
        .lock()
        .expect("Neon service")
        .queue_rolls([0.0]);
    provider
        .lobby_join_observer()
        .gamba_spectator(LobbyGambaSpectator {
            guild_id: 42,
            player_id: 101,
            player_display_name: "Spectator".to_owned(),
            channel_id: 75,
        })
        .await
        .expect("gamba spectator Neon");

    let state = discord.state.lock().expect("Discord state");
    assert_eq!(state.channel_messages.len(), 2);
    assert_eq!(state.channel_messages[0].0, 70);
    assert!(
        state.channel_messages[0]
            .1
            .response
            .content
            .contains("Player 100")
    );
    assert!(state.channel_messages[0].1.response.content.contains('7'));
    assert_eq!(state.channel_messages[1].0, 75);
    assert!(
        state.channel_messages[1]
            .1
            .response
            .content
            .contains("Spectator")
    );
    assert!(state.channel_messages.iter().all(|(_, message)| {
        message.allowed_mentions == crate::discord_transport::DiscordAllowedMentions::None
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roles_deduplicate_in_order_commit_atomically_and_reply_publicly() {
    let database = database();
    let connection = Connection::open(database.path()).expect("open roles database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production SQLite policy");
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username) VALUES (300,42,'Roles')",
            [],
        )
        .expect("seed roles player");
    let server = RouteServer::start(Vec::new());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        Arc::new(MockDiscord::default()),
        config(),
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("player")
        .expect("player handler")
        .handle(
            player_command(
                301,
                300,
                "roles",
                vec![option(
                    "roles",
                    InteractionValue::String("54321123".to_owned()),
                )],
            ),
            responder.clone(),
        )
        .await
        .expect("set roles");
    let captured = responder.captured.lock().expect("roles response");
    assert_eq!(captured.deferred, [true]);
    assert_eq!(captured.followups.len(), 1);
    assert!(!captured.followups[0].ephemeral);
    assert_eq!(
        captured.followups[0].content,
        "✅ Set your preferred roles to: ⚕️ Hard Support, 🔥 Soft Support, 🛡️ Offlane, 🏹 Mid, ⚔️ Carry"
    );
    drop(captured);
    assert_eq!(
        RegistrationRepository::new(database.path())
            .get_roles(300, Some(42))
            .expect("read roles")
            .expect("registered player roles"),
        ["5", "4", "3", "2", "1"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_mmr_attempts_and_timeout_are_restart_safe() {
    let database = database();
    let server = RouteServer::start(vec![r#"{"mmr_estimate":{"estimate":null}}"#]);
    let discord = Arc::new(MockDiscord::default());
    let provider = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord.clone(),
        config(),
    );
    let initial_registry = registry(&provider);
    let prompt = Arc::new(CapturingResponder::default());
    initial_registry
        .command_handler("player")
        .expect("player handler")
        .handle(
            player_command(
                500,
                400,
                "register",
                vec![option("steam_id", InteractionValue::Integer(44_444))],
            ),
            prompt.clone(),
        )
        .await
        .expect("MMR prompt");
    let button_id = prompt.captured.lock().expect("prompt response").followups[0].components[0]
        .buttons[0]
        .custom_id
        .clone();
    let modal_id = button_id.replace(":button:", ":modal:");
    for value in ["", "not-a-number", "0", "12001"] {
        let invalid = Arc::new(CapturingResponder::default());
        initial_registry
            .component_handler(&modal_id)
            .expect("modal handler")
            .handle(
                InteractionRequest::Modal {
                    interaction_id: 501,
                    custom_id: modal_id.clone(),
                    user_id: 400,
                    guild_id: Some(42),
                    channel_id: Some(9),
                    member_permissions: None,
                    fields: BTreeMap::from([("mmr".to_owned(), value.to_owned())]),
                },
                invalid.clone(),
            )
            .await
            .expect("invalid MMR response");
        assert_eq!(
            invalid.captured.lock().expect("invalid response").immediate[0].content,
            "❌ Invalid MMR"
        );
    }
    let restarted = PlayerRegistrationProvider::with_config(
        database.path(),
        services(&server),
        discord,
        config(),
    );
    let blocked = Arc::new(CapturingResponder::default());
    registry(&restarted)
        .component_handler(&button_id)
        .expect("button handler")
        .handle(
            InteractionRequest::Component {
                interaction_id: 502,
                custom_id: button_id.clone(),
                user_id: 400,
                user_display_name: "Registration Player 400".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                values: Vec::new(),
            },
            blocked.clone(),
        )
        .await
        .expect("retry exhaustion modal");
    let blocked_modal_id = blocked.captured.lock().expect("blocked modal").modals[0]
        .custom_id
        .clone();
    let blocked_submit = Arc::new(CapturingResponder::default());
    registry(&restarted)
        .component_handler(&blocked_modal_id)
        .expect("blocked modal handler")
        .handle(
            InteractionRequest::Modal {
                interaction_id: 503,
                custom_id: blocked_modal_id,
                user_id: 400,
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                fields: BTreeMap::from([("mmr".to_owned(), "4000".to_owned())]),
            },
            blocked_submit.clone(),
        )
        .await
        .expect("retry exhaustion response");
    assert_eq!(
        blocked_submit
            .captured
            .lock()
            .expect("blocked response")
            .immediate[0]
            .content,
        "❌ Invalid MMR"
    );
    assert_eq!(
        RegistrationRepository::new(database.path())
            .mmr_prompt(42, 500)
            .expect("MMR state")
            .expect("persisted MMR state")
            .attempts,
        3
    );

    let repository = RegistrationRepository::new(database.path());
    repository
        .save_mmr_prompt(&MmrPromptState {
            nonce: 600,
            discord_id: 401,
            guild_id: 42,
            steam_id: 55_555,
            attempts: 0,
            expires_at: now_seconds() - 1,
            completed: false,
        })
        .expect("save expired MMR state");
    let expired = Arc::new(CapturingResponder::default());
    registry(&restarted)
        .component_handler("registration:mmr:button:42:600")
        .expect("expired button route")
        .handle(
            InteractionRequest::Component {
                interaction_id: 601,
                custom_id: "registration:mmr:button:42:600".to_owned(),
                user_id: 401,
                user_display_name: "Registration Player 401".to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                values: Vec::new(),
            },
            expired.clone(),
        )
        .await
        .expect("expired prompt modal");
    let expired_modal_id = expired.captured.lock().expect("expired modal").modals[0]
        .custom_id
        .clone();
    let expired_submit = Arc::new(CapturingResponder::default());
    registry(&restarted)
        .component_handler(&expired_modal_id)
        .expect("expired modal handler")
        .handle(
            InteractionRequest::Modal {
                interaction_id: 602,
                custom_id: expired_modal_id,
                user_id: 401,
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                fields: BTreeMap::from([("mmr".to_owned(), "4000".to_owned())]),
            },
            expired_submit.clone(),
        )
        .await
        .expect("expired prompt response");
    assert_eq!(
        expired_submit
            .captured
            .lock()
            .expect("expired response")
            .immediate[0]
            .content,
        "❌ Invalid MMR"
    );
}
