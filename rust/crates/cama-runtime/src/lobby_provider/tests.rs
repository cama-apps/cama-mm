use super::*;

use std::sync::Mutex;

use crate::discord_transport::{
    DiscordGuildMemberSnapshot, DiscordMessageReceipt, DiscordMessageSnapshot, DiscordUserSnapshot,
};
use crate::gateway_events::{GatewayMember, GuildMemberPageSource};
use crate::raw_reactions::RawReactionEmoji;
use crate::registration::{InteractionAllowedMentions, InteractionResponseError, Registry};
use cama_app::moderation::{CreateSuspension, ModerationService};
use cama_db::core_repositories::NewPlayer;
use cama_db::low_priority_repository::{LowPriorityRepository, SetLowPriorityInput};
use cama_db::moderation::{
    ModerationRepository, ModerationSource, SuspensionCompletion, SuspensionScope,
};
use cama_db::schema_manager::initialize_or_migrate;
use cama_domain::curfew::CurfewWindow;
use chrono::Timelike;
use tempfile::NamedTempFile;

#[derive(Default)]
struct CapturedResponses {
    deferred: Vec<bool>,
    followups: Vec<InteractionResponse>,
}

#[tokio::test]
async fn test_addfake_adds_users_to_lobby() {
    let database = database_with_players(&[]);
    let provider = provider_for(&database, Arc::new(RecordingTransport::default()));

    let result = provider
        .admin_control()
        .add_fake_users(fake_request(5, 1))
        .await
        .expect("add fake users");

    assert_eq!(result.users_added, 5);
    assert_eq!(
        lobby_snapshot(&provider, LobbyKind::Open).players,
        (-5..=-1).map(AppUserId).collect()
    );
}

#[tokio::test]
async fn test_addfake_continues_numbering() {
    let database = database_with_players(&[]);
    let provider = provider_for(&database, Arc::new(RecordingTransport::default()));
    let control = provider.admin_control();

    let first = control
        .add_fake_users(fake_request(3, 1))
        .await
        .expect("first fake batch");
    let second = control
        .add_fake_users(fake_request(3, 2))
        .await
        .expect("second fake batch");

    assert_eq!(first.user_names, ["FakeUser1", "FakeUser2", "FakeUser3"]);
    assert_eq!(second.user_names, ["FakeUser4", "FakeUser5", "FakeUser6"]);
    assert_eq!(
        lobby_snapshot(&provider, LobbyKind::Open).players,
        (-6..=-1).map(AppUserId).collect()
    );
}

#[tokio::test]
async fn test_addfake_refreshes_partial_lobby_message_without_fetch() {
    let database = database_with_players(&[(10, "Creator")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(&provider, "lobby", 10, "Creator", Vec::new()).await;
    let fetches_before = transport.fetch_count();
    let edits_before = transport.edit_count();

    provider
        .admin_control()
        .add_fake_users(fake_request(1, 2))
        .await
        .expect("add fake user and refresh");

    assert_eq!(transport.fetch_count(), fetches_before);
    assert_eq!(transport.edit_count(), edits_before + 1);
}

#[tokio::test]
async fn test_filllobby_refreshes_partial_lobby_message_without_fetch() {
    let database = database_with_players(&[(10, "Creator")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(&provider, "lobby", 10, "Creator", Vec::new()).await;
    let fetches_before = transport.fetch_count();
    let edits_before = transport.edit_count();

    provider
        .admin_control()
        .fill_lobby(fake_request(0, 2))
        .await
        .expect("fill lobby and refresh");

    assert_eq!(
        lobby_snapshot(&provider, LobbyKind::Open).players.len(),
        runtime_config().ready_threshold
    );
    assert_eq!(transport.fetch_count(), fetches_before);
    assert_eq!(transport.edit_count(), edits_before + 1);
}

#[derive(Default)]
struct CapturingResponder {
    captured: Mutex<CapturedResponses>,
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture lock")
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
            .expect("response capture lock")
            .followups
            .push(response);
        Ok(())
    }
}

#[derive(Clone)]
struct SentMessage {
    channel_id: u64,
    message: DiscordMessage,
}

struct RecordingState {
    next_message_id: u64,
    messages: BTreeMap<(u64, u64), DiscordMessageSnapshot>,
    fetches: Vec<(u64, u64)>,
    sent: Vec<SentMessage>,
    edits: Vec<(u64, u64, DiscordMessage)>,
    deleted: Vec<(u64, u64)>,
    threads: Vec<(u64, u64, String, u64)>,
    thread_members: Vec<(u64, u64)>,
    archived: Vec<(u64, String, bool)>,
    unpinned: Vec<(u64, u64)>,
    removed_reactions: Vec<(u64, u64, DiscordEmoji, u64)>,
    cleared_reactions: Vec<(u64, u64, DiscordEmoji)>,
    direct_messages: Vec<(u64, DiscordMessage)>,
    members: BTreeMap<(u64, u64), DiscordGuildMemberSnapshot>,
    users: BTreeMap<u64, DiscordUserSnapshot>,
}

impl Default for RecordingState {
    fn default() -> Self {
        Self {
            next_message_id: 1_000,
            messages: BTreeMap::new(),
            fetches: Vec::new(),
            sent: Vec::new(),
            edits: Vec::new(),
            deleted: Vec::new(),
            threads: Vec::new(),
            thread_members: Vec::new(),
            archived: Vec::new(),
            unpinned: Vec::new(),
            removed_reactions: Vec::new(),
            cleared_reactions: Vec::new(),
            direct_messages: Vec::new(),
            members: BTreeMap::new(),
            users: BTreeMap::new(),
        }
    }
}

#[derive(Default)]
struct RecordingTransport {
    state: Mutex<RecordingState>,
}

impl RecordingTransport {
    fn thread_count(&self) -> usize {
        self.state.lock().expect("transport state").threads.len()
    }

    fn sent_messages(&self) -> Vec<SentMessage> {
        self.state.lock().expect("transport state").sent.clone()
    }

    fn edit_count(&self) -> usize {
        self.state.lock().expect("transport state").edits.len()
    }

    fn fetch_count(&self) -> usize {
        self.state.lock().expect("transport state").fetches.len()
    }

    fn seed_reaction(&self, channel_id: u64, message_id: u64, emoji: DiscordEmoji) {
        self.state
            .lock()
            .expect("transport state")
            .messages
            .get_mut(&(channel_id, message_id))
            .expect("seeded message")
            .reactions
            .push(emoji);
    }

    fn set_member(&self, guild_id: u64, member: DiscordGuildMemberSnapshot) {
        self.state
            .lock()
            .expect("transport state")
            .members
            .insert((guild_id, member.user_id), member);
    }

    fn set_user(&self, user: DiscordUserSnapshot) {
        self.state
            .lock()
            .expect("transport state")
            .users
            .insert(user.user_id, user);
    }
}

#[async_trait]
impl DiscordTransport for RecordingTransport {
    async fn fetch_message(
        &self,
        channel_id: u64,
        message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        let mut state = self.state.lock().expect("transport state");
        state.fetches.push((channel_id, message_id));
        Ok(state.messages.get(&(channel_id, message_id)).cloned())
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        let mut state = self.state.lock().expect("transport state");
        let message_id = state.next_message_id;
        state.next_message_id += 1;
        let receipt = DiscordMessageReceipt {
            channel_id,
            message_id,
            jump_url: format!("https://discord.com/channels/42/{channel_id}/{message_id}"),
        };
        state.messages.insert(
            (channel_id, message_id),
            DiscordMessageSnapshot {
                receipt: receipt.clone(),
                reactions: Vec::new(),
            },
        );
        state.sent.push(SentMessage {
            channel_id,
            message,
        });
        Ok(receipt)
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.state
            .lock()
            .expect("transport state")
            .edits
            .push((channel_id, message_id, message));
        Ok(())
    }

    async fn delete_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        let mut state = self.state.lock().expect("transport state");
        state.messages.remove(&(channel_id, message_id));
        state.deleted.push((channel_id, message_id));
        Ok(())
    }

    async fn create_public_thread(
        &self,
        channel_id: u64,
        message_id: u64,
        name: &str,
    ) -> Result<u64, String> {
        let thread_id = 20_000 + message_id;
        self.state.lock().expect("transport state").threads.push((
            channel_id,
            message_id,
            name.to_owned(),
            thread_id,
        ));
        Ok(thread_id)
    }

    async fn add_thread_member(&self, thread_id: u64, member_id: u64) -> Result<(), String> {
        self.state
            .lock()
            .expect("transport state")
            .thread_members
            .push((thread_id, member_id));
        Ok(())
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(&self, thread_id: u64, name: &str, locked: bool) -> Result<(), String> {
        self.state.lock().expect("transport state").archived.push((
            thread_id,
            name.to_owned(),
            locked,
        ));
        Ok(())
    }

    async fn add_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        if let Some(message) = self
            .state
            .lock()
            .expect("transport state")
            .messages
            .get_mut(&(channel_id, message_id))
        {
            message.reactions.push(emoji.clone());
        }
        Ok(())
    }

    async fn remove_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
        user_id: u64,
    ) -> Result<(), String> {
        self.state
            .lock()
            .expect("transport state")
            .removed_reactions
            .push((channel_id, message_id, emoji.clone(), user_id));
        Ok(())
    }

    async fn clear_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        let mut state = self.state.lock().expect("transport state");
        if let Some(message) = state.messages.get_mut(&(channel_id, message_id)) {
            message.reactions.retain(|existing| existing != emoji);
        }
        state
            .cleared_reactions
            .push((channel_id, message_id, emoji.clone()));
        Ok(())
    }

    async fn unpin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        self.state
            .lock()
            .expect("transport state")
            .unpinned
            .push((channel_id, message_id));
        Ok(())
    }

    async fn send_direct_message(
        &self,
        user_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.state
            .lock()
            .expect("transport state")
            .direct_messages
            .push((user_id, message));
        Ok(())
    }

    async fn guild_member(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        Ok(self
            .state
            .lock()
            .expect("transport state")
            .members
            .get(&(guild_id, user_id))
            .cloned())
    }

    async fn user(&self, user_id: u64) -> Result<Option<DiscordUserSnapshot>, String> {
        Ok(self
            .state
            .lock()
            .expect("transport state")
            .users
            .get(&user_id)
            .cloned())
    }
}

struct NoMembers;

#[async_trait]
impl GuildMemberPageSource for NoMembers {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct RecordingJoinObserver {
    confirmed: Mutex<Vec<ConfirmedLobbyJoin>>,
    explicit_neon: Mutex<Vec<ConfirmedLobbyJoin>>,
    gamba: Mutex<Vec<LobbyGambaSpectator>>,
    resets: Mutex<Vec<(u64, LobbyKind)>>,
}

#[async_trait]
impl LobbyJoinObserver for RecordingJoinObserver {
    async fn confirmed_lobby_join(&self, event: ConfirmedLobbyJoin) -> Result<(), String> {
        self.confirmed.lock().expect("join observer").push(event);
        Ok(())
    }

    async fn explicit_lobby_join_neon(&self, event: ConfirmedLobbyJoin) -> Result<(), String> {
        self.explicit_neon
            .lock()
            .expect("join observer")
            .push(event);
        Ok(())
    }

    async fn gamba_spectator(&self, event: LobbyGambaSpectator) -> Result<(), String> {
        self.gamba.lock().expect("join observer").push(event);
        Ok(())
    }

    async fn lobby_reset(&self, guild_id: u64, lobby_kind: LobbyKind) -> Result<(), String> {
        self.resets
            .lock()
            .expect("reset observer")
            .push((guild_id, lobby_kind));
        Ok(())
    }
}

#[test]
fn registration_exposes_the_complete_lobby_command_surface() {
    struct UnusedTransport;

    #[async_trait]
    impl DiscordTransport for UnusedTransport {
        async fn fetch_message(
            &self,
            _channel_id: u64,
            _message_id: u64,
        ) -> Result<Option<crate::discord_transport::DiscordMessageSnapshot>, String> {
            Ok(None)
        }

        async fn send_message(
            &self,
            _channel_id: u64,
            _message: DiscordMessage,
        ) -> Result<crate::discord_transport::DiscordMessageReceipt, String> {
            Err("unused transport".to_owned())
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
            Err("unused transport".to_owned())
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
            _user_id: u64,
            _message: DiscordMessage,
        ) -> Result<(), String> {
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

    let database = tempfile::NamedTempFile::new().expect("temporary lobby database");
    cama_db::schema_manager::initialize_or_migrate(database.path())
        .expect("initialize Python-compatible schema");
    let provider = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::new(),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 100,
        },
        Arc::new(DraftStateManager::default()),
        Arc::new(UnusedTransport),
    )
    .expect("construct lobby provider");
    let mut builder = RegistryBuilder::default();
    provider
        .register(&mut builder)
        .expect("register lobby commands");
    let registry = builder.build();
    let commands = registry
        .commands()
        .map(|command| command.name.as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        commands,
        BTreeSet::from(["join", "kick", "leave", "lobby", "readycheck", "resetlobby"])
    );
}

fn runtime_config() -> LobbyRuntimeConfig {
    LobbyRuntimeConfig {
        lobby_channel_id: Some(700),
        low_skill_lobby_channel_id: Some(701),
        admin_user_ids: BTreeSet::new(),
        ready_threshold: 2,
        max_players: 20,
        first_game_pool_daily_amount: 100,
    }
}

fn database_with_players(players: &[(i64, &str)]) -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary lobby database");
    initialize_or_migrate(database.path()).expect("initialize Python-compatible schema");
    let repository = PlayerRepository::new(database.path());
    for (player_id, name) in players {
        let mut player = NewPlayer::new(*player_id, *name, Some(42));
        player.preferred_roles = Some(vec!["1".to_owned(), "5".to_owned()]);
        player.glicko_rating = Some(1_000.0);
        repository.add(&player).expect("insert registered player");
    }
    database
}

fn provider_for(
    database: &NamedTempFile,
    transport: Arc<RecordingTransport>,
) -> LobbyRegistrationProvider {
    LobbyRegistrationProvider::new(
        database.path(),
        runtime_config(),
        Arc::new(DraftStateManager::default()),
        transport,
    )
    .expect("construct live lobby provider")
}

fn provider_for_admin(
    database: &NamedTempFile,
    transport: Arc<RecordingTransport>,
    admin_user_id: u64,
) -> LobbyRegistrationProvider {
    let mut config = runtime_config();
    config.admin_user_ids.insert(admin_user_id);
    LobbyRegistrationProvider::new(
        database.path(),
        config,
        Arc::new(DraftStateManager::default()),
        transport,
    )
    .expect("construct live lobby provider with administrator")
}

fn registry_for(provider: &LobbyRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    provider
        .register(&mut builder)
        .expect("register lobby provider");
    builder.build()
}

fn lobby_option(kind: LobbyKind) -> InteractionOption {
    InteractionOption {
        name: "lobby".to_owned(),
        value: InteractionValue::String(lobby_kind_value(kind).to_owned()),
    }
}

fn player_option(player_id: u64, display_name: &str) -> InteractionOption {
    InteractionOption {
        name: "player".to_owned(),
        value: InteractionValue::User {
            id: player_id,
            display_name: Some(display_name.to_owned()),
            is_bot: Some(false),
        },
    }
}

fn reason_option(reason: &str) -> InteractionOption {
    InteractionOption {
        name: "reason".to_owned(),
        value: InteractionValue::String(reason.to_owned()),
    }
}

async fn dispatch_command(
    provider: &LobbyRegistrationProvider,
    name: &str,
    user_id: u64,
    display_name: &str,
    options: Vec<InteractionOption>,
) -> Arc<CapturingResponder> {
    let responder = Arc::new(CapturingResponder::default());
    registry_for(provider)
        .command_handler(name)
        .expect("registered lobby handler")
        .handle(
            InteractionRequest::Command {
                interaction_id: user_id,
                name: name.to_owned(),
                user_id,
                user_display_name: display_name.to_owned(),
                guild_id: Some(42),
                channel_id: Some(9),
                member_permissions: None,
                options,
            },
            responder.clone(),
        )
        .await
        .expect("dispatch lobby command");
    responder
}

async fn create_lobby_and_join_player(
    provider: &LobbyRegistrationProvider,
    kind: LobbyKind,
    creator_id: u64,
    creator_name: &str,
    player_id: u64,
    player_name: &str,
) {
    dispatch_command(
        provider,
        "lobby",
        creator_id,
        creator_name,
        vec![lobby_option(kind)],
    )
    .await;
    let message_id = to_u64(
        lobby_snapshot(provider, kind)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord message id");
    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Add,
            message_id,
            player_id,
            player_name,
        ))
        .await
        .expect("player joins lobby");
}

fn lobby_snapshot(provider: &LobbyRegistrationProvider, kind: LobbyKind) -> LobbySnapshot {
    provider
        .handler
        .state
        .service
        .get_lobby(LobbyScope::new(AppGuildId(42), kind))
        .expect("persisted lobby snapshot")
}

fn fake_request(count: usize, interaction_id: u64) -> AdminFakeLobbyRequest {
    AdminFakeLobbyRequest {
        interaction_id,
        guild_id: 42,
        channel_id: 9,
        count,
    }
}

fn raw_sword(
    kind: RawReactionKind,
    message_id: u64,
    user_id: u64,
    display_name: &str,
) -> RawReactionEvent {
    RawReactionEvent {
        kind,
        guild_id: Some(42),
        channel_id: 700,
        message_id,
        user_id,
        actor_is_bot: Some(false),
        actor_display_name: Some(display_name.to_owned()),
        emoji: RawReactionEmoji::unicode(SWORD_EMOJI),
    }
}

#[tokio::test]
async fn live_slash_creation_persists_and_restart_reuses_the_existing_discord_message() {
    let database = database_with_players(&[(10, "Creator")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());

    let responder = dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let first = lobby_snapshot(&provider, LobbyKind::Open);
    assert_eq!(first.players, BTreeSet::from([AppUserId(10)]));
    assert_eq!(first.message_ids.channel_id, Some(AppChannelId(700)));
    assert!(first.message_ids.message_id.is_some());
    assert!(first.message_ids.thread_id.is_some());
    assert_eq!(transport.thread_count(), 1);
    {
        let captured = responder.captured.lock().expect("responses");
        assert_eq!(captured.deferred, [false]);
        assert!(captured.followups.iter().all(|response| {
            response.ephemeral && response.allowed_mentions == InteractionAllowedMentions::None
        }));
    }

    let restarted = provider_for(&database, transport.clone());
    assert_eq!(
        lobby_snapshot(&restarted, LobbyKind::Open).message_ids,
        first.message_ids
    );
    dispatch_command(
        &restarted,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    assert_eq!(
        transport.thread_count(),
        1,
        "restart must reuse the persisted message/thread instead of duplicating the lobby"
    );
}

#[tokio::test]
async fn slash_join_uses_only_the_private_confirmation_message() {
    let database = database_with_players(&[(10, "Creator"), (20, "Joiner")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    let observer = Arc::new(RecordingJoinObserver::default());
    provider
        .set_join_observer(observer.clone())
        .expect("install join observer");

    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let messages_before_join = transport.sent_messages().len();

    let responder = dispatch_command(
        &provider,
        "join",
        20,
        "Joiner",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let captured = responder.captured.lock().expect("responses");
    assert_eq!(captured.followups.len(), 1);
    assert_eq!(
        captured.followups[0].content,
        "✅ Joined 🍽️ All You Can Feed!"
    );
    assert!(captured.followups[0].ephemeral);
    assert_eq!(transport.sent_messages().len(), messages_before_join);
    assert_eq!(
        transport
            .state
            .lock()
            .expect("transport state")
            .thread_members,
        [(21_000, 20)]
    );
    assert_eq!(observer.confirmed.lock().expect("join observer").len(), 2);
}

#[tokio::test]
async fn test_lobby_state_restored_after_restart() {
    let database = database_with_players(&[
        (10, "Creator"),
        (20, "Second"),
        (30, "Third"),
        (40, "Fourth"),
    ]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let initial_message = lobby_snapshot(&provider, LobbyKind::Open)
        .message_ids
        .message_id
        .expect("persisted lobby message");
    let message_id = to_u64(initial_message.0).expect("Discord lobby message id");
    for (player_id, player_name) in [(20, "Second"), (30, "Third"), (40, "Fourth")] {
        provider
            .raw_reaction_observer()
            .observe(raw_sword(
                RawReactionKind::Add,
                message_id,
                player_id,
                player_name,
            ))
            .await
            .expect("persist lobby member");
    }
    let before = lobby_snapshot(&provider, LobbyKind::Open);
    assert_eq!(before.created_by, Some(AppUserId(10)));
    assert_eq!(
        before.players,
        BTreeSet::from([AppUserId(10), AppUserId(20), AppUserId(30), AppUserId(40),])
    );

    let restarted = provider_for(&database, transport);
    let after = lobby_snapshot(&restarted, LobbyKind::Open);
    assert_eq!(after.created_by, before.created_by);
    assert_eq!(after.players, before.players);
    assert_eq!(after.message_ids, before.message_ids);
    assert_eq!(after.player_join_times, before.player_join_times);
}

#[tokio::test]
async fn raw_sword_routes_are_kind_scoped_dual_seat_persistent_and_mention_safe() {
    let database = database_with_players(&[(10, "Creator"), (20, "Dual Queue")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        dispatch_command(&provider, "lobby", 10, "Creator", vec![lobby_option(kind)]).await;
        let message_id = to_u64(
            lobby_snapshot(&provider, kind)
                .message_ids
                .message_id
                .expect("lobby message")
                .0,
        )
        .expect("Discord message id");
        provider
            .raw_reaction_observer()
            .observe(raw_sword(
                RawReactionKind::Add,
                message_id,
                20,
                "Dual Queue",
            ))
            .await
            .expect("raw sword join");
    }

    assert_eq!(
        provider
            .handler
            .state
            .service
            .get_lobby_kinds_for_player(AppUserId(20), AppGuildId(42)),
        vec![LobbyKind::Open, LobbyKind::LowSkill]
    );
    let open_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("open message")
            .0,
    )
    .expect("open message id");
    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Remove,
            open_message_id,
            20,
            "Dual Queue",
        ))
        .await
        .expect("raw sword leave");
    assert_eq!(
        provider
            .handler
            .state
            .service
            .get_lobby_kinds_for_player(AppUserId(20), AppGuildId(42)),
        vec![LobbyKind::LowSkill]
    );

    let restarted = provider_for(&database, transport.clone());
    assert_eq!(
        restarted
            .handler
            .state
            .service
            .get_lobby_kinds_for_player(AppUserId(20), AppGuildId(42)),
        vec![LobbyKind::LowSkill],
        "raw add/remove mutations must survive process restart"
    );
    for sent in transport.sent_messages() {
        if sent.message.response.content.contains("<@") {
            let DiscordAllowedMentions::Users(users) = sent.message.allowed_mentions else {
                panic!("message containing a user tag must carry an explicit user allowlist");
            };
            assert!(!users.is_empty());
            assert!(users.iter().all(|user_id| [10, 20].contains(user_id)));
        }
    }
}

#[tokio::test]
async fn raw_jopacoin_subscribes_the_thread_then_invokes_the_shared_neon_observer() {
    let database = database_with_players(&[(10, "Creator"), (20, "Spectator")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    let observer = Arc::new(RecordingJoinObserver::default());
    provider
        .set_join_observer(observer.clone())
        .expect("attach shared observer");
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let lobby = lobby_snapshot(&provider, LobbyKind::Open);
    let message_id = to_u64(lobby.message_ids.message_id.expect("lobby message").0)
        .expect("Discord lobby message");
    transport.set_user(DiscordUserSnapshot {
        user_id: 20,
        display_name: "Fetched Spectator".to_owned(),
        is_bot: false,
    });
    let reaction = |user_id, name: Option<&str>| RawReactionEvent {
        kind: RawReactionKind::Add,
        guild_id: Some(42),
        channel_id: 700,
        message_id,
        user_id,
        actor_is_bot: Some(false),
        actor_display_name: name.map(str::to_owned),
        emoji: RawReactionEmoji::custom(JOPACOIN_EMOJI_ID, "jopacoin", false),
    };

    provider
        .raw_reaction_observer()
        .observe(reaction(20, None))
        .await
        .expect("gamba reaction");
    let gamba = observer.gamba.lock().expect("gamba events").clone();
    assert_eq!(
        gamba,
        vec![LobbyGambaSpectator {
            guild_id: 42,
            player_id: 20,
            player_display_name: "Fetched Spectator".to_owned(),
            channel_id: 700,
        }]
    );
    let sent = transport.sent_messages();
    assert!(sent.iter().any(|sent| {
        sent.message.response.content == format!("{JOPACOIN_EMOTE} <@20> is here for the gamba!")
            && sent.message.allowed_mentions == DiscordAllowedMentions::Users(BTreeSet::from([20]))
    }));

    provider
        .raw_reaction_observer()
        .observe(reaction(10, Some("Creator")))
        .await
        .expect("seated player gamba is ignored");
    assert_eq!(observer.gamba.lock().expect("gamba events").len(), 1);
}

#[tokio::test]
async fn suspension_rejects_slash_and_raw_with_private_context_while_low_priority_still_joins() {
    let database =
        database_with_players(&[(10, "Creator"), (20, "Suspended"), (30, "Low Priority")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let lobby_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord lobby message");
    let now = unix_time_now() as i64;
    ModerationService::new(ModerationRepository::new(database.path()))
        .create_suspension(CreateSuspension {
            discord_id: 20,
            guild_id: Some(42),
            actor_id: 900,
            reason: "Take a matchmaking break",
            scope: SuspensionScope::Open,
            completion: SuspensionCompletion::Time,
            expires_at: Some(now + 3_600),
            matches: None,
            source: ModerationSource::Admin,
            replace: false,
            pending_match_watermark: None,
            now,
        })
        .expect("seed active lobby suspension");
    LowPriorityRepository::new(database.path())
        .set_low_priority(&SetLowPriorityInput::new(
            30,
            Some(42),
            900,
            Some("Play low-priority matches".to_owned()),
        ))
        .expect("seed active low-priority state");

    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Add,
            lobby_message_id,
            20,
            "Suspended",
        ))
        .await
        .expect("raw suspension rejection");
    assert!(
        !lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(20))
    );
    {
        let state = transport.state.lock().expect("transport state");
        assert!(
            state
                .removed_reactions
                .iter()
                .any(|(_, message_id, emoji, user_id)| {
                    *message_id == lobby_message_id && emoji.name == SWORD_EMOJI && *user_id == 20
                })
        );
        let (recipient, direct) = state.direct_messages.last().expect("suspension DM");
        assert_eq!(*recipient, 20);
        assert_eq!(
            direct.response.content,
            "You are temporarily suspended from this matchmaking lobby.\nReason: Take a matchmaking break\nUse `/player lobby status` in the server for the exact remaining term."
        );
        let public = state.sent.last().expect("public suspension rejection");
        assert_eq!(
            public.message.response.content,
            "<@20> ❌ You are temporarily restricted from this matchmaking lobby. Check your DMs or use `/player lobby status`."
        );
        assert_eq!(
            public.message.allowed_mentions,
            DiscordAllowedMentions::Users(BTreeSet::from([20]))
        );
    }

    let slash = dispatch_command(
        &provider,
        "join",
        20,
        "Suspended",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    {
        let captured = slash.captured.lock().expect("slash responses");
        assert_eq!(captured.deferred, [true]);
        let response = captured.followups.last().expect("slash rejection");
        assert!(response.ephemeral);
        assert!(
            response
                .content
                .contains("suspended from 🍽️ All You Can Feed")
        );
        assert!(
            response
                .content
                .contains("Reason: Take a matchmaking break")
        );
    }

    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Add,
            lobby_message_id,
            30,
            "Low Priority",
        ))
        .await
        .expect("low-priority raw join");
    assert!(
        lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(30)),
        "Python low-priority state changes shuffle/rating policy but is not a join rejection"
    );
}

#[tokio::test]
async fn ready_recovery_repaints_persisted_lobbies_and_removes_the_legacy_reaction() {
    let database = database_with_players(&[(10, "Creator")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let lobby = lobby_snapshot(&provider, LobbyKind::Open);
    let channel_id = to_u64(lobby.message_ids.channel_id.expect("channel").0).expect("channel id");
    let message_id = to_u64(lobby.message_ids.message_id.expect("message").0).expect("message id");
    transport.seed_reaction(
        channel_id,
        message_id,
        DiscordEmoji {
            name: "frogling".to_owned(),
            id: Some(LEGACY_FROGLING_EMOJI_ID),
        },
    );
    let edits_before = transport.edit_count();

    let report = provider
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42]),
            Arc::new(NoMembers),
        ))
        .await;
    assert_eq!(report.observer, "lobby-message-reconciliation");
    assert_eq!(report.guilds_attempted, 1);
    assert_eq!(report.guilds_refreshed, 1);
    assert!(report.failures.is_empty());
    let state = transport.state.lock().expect("transport state");
    assert!(state.edits.len() > edits_before);
    assert!(
        state
            .cleared_reactions
            .iter()
            .any(|(_, _, emoji)| { emoji.id == Some(LEGACY_FROGLING_EMOJI_ID) })
    );
}

#[tokio::test]
async fn inferred_creator_reset_runs_cleanup_and_clears_the_existing_schema_row() {
    let database = database_with_players(&[(10, "Creator")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    let observer = Arc::new(RecordingJoinObserver::default());
    provider
        .set_join_observer(observer.clone())
        .expect("attach reset observer");
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let responder = dispatch_command(&provider, "resetlobby", 10, "Creator", Vec::new()).await;
    assert!(
        provider
            .handler
            .state
            .service
            .get_lobby(LobbyScope::new(AppGuildId(42), LobbyKind::Open))
            .is_none()
    );
    assert!(
        ReadycheckRepository::new(database.path())
            .load(
                LobbyScope::new(AppGuildId(42), LobbyKind::Open).lobby_id(),
                Some(42),
            )
            .expect("read cleared lobby row")
            .is_none()
    );
    let state = transport.state.lock().expect("transport state");
    assert_eq!(state.archived.len(), 1);
    assert_eq!(state.unpinned.len(), 1);
    drop(state);
    assert_eq!(
        *observer.resets.lock().expect("reset observer"),
        vec![(42, LobbyKind::Open)]
    );
    let captured = responder.captured.lock().expect("responses");
    assert_eq!(captured.deferred, [true]);
    assert!(captured.followups.iter().any(|response| {
        response.ephemeral
            && response.allowed_mentions == InteractionAllowedMentions::Default
            && response.content.to_lowercase().contains("reset")
    }));
}

#[test]
fn test_kick_reason_is_optional_and_privately_bounded() {
    let database = database_with_players(&[]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport);
    let registry = registry_for(&provider);
    let kick = registry
        .commands()
        .find(|command| command.name == "kick")
        .expect("registered kick command");
    let reason = kick
        .options
        .iter()
        .find(|option| option.name == "reason")
        .expect("optional kick reason");

    assert!(!reason.required);
    assert_eq!(reason.min_length, Some(3));
    assert_eq!(reason.max_length, Some(300));
}

#[tokio::test]
async fn test_kick_removes_reaction_and_updates_message() {
    let database = database_with_players(&[(1, "Admin"), (42, "Target")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for_admin(&database, transport.clone(), 1);
    create_lobby_and_join_player(&provider, LobbyKind::Open, 1, "Admin", 42, "Target").await;
    let (edits_before, removals_before) = {
        let state = transport.state.lock().expect("transport state");
        (state.edits.len(), state.removed_reactions.len())
    };

    let responder = dispatch_command(
        &provider,
        "kick",
        1,
        "Admin",
        vec![player_option(42, "Target")],
    )
    .await;

    let state = transport.state.lock().expect("transport state");
    assert!(state.edits.len() > edits_before);
    assert!(state.removed_reactions.len() > removals_before);
    drop(state);
    let captured = responder.captured.lock().expect("responses");
    assert_eq!(captured.deferred, [true]);
    assert!(captured.followups.iter().any(|response| {
        response.ephemeral
            && response.content == format!("✅ Kicked <@42> from {}.", LobbyKind::Open.label())
    }));
}

#[tokio::test]
async fn test_creator_who_left_lobby_can_no_longer_kick() {
    let database = database_with_players(&[(7, "Creator"), (42, "Target")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport);
    create_lobby_and_join_player(&provider, LobbyKind::Open, 7, "Creator", 42, "Target").await;
    assert!(
        provider
            .handler
            .state
            .service
            .try_leave_lobby(
                AppUserId(7),
                LobbyScope::new(AppGuildId(42), LobbyKind::Open),
            )
            .expect("creator leaves lobby")
    );

    let responder = dispatch_command(
        &provider,
        "kick",
        7,
        "Creator",
        vec![player_option(42, "Target")],
    )
    .await;

    assert!(
        lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(42))
    );
    let captured = responder.captured.lock().expect("responses");
    assert!(
        captured.followups.iter().any(|response| {
            response.ephemeral && response.content.contains("Permission denied")
        })
    );
}

#[tokio::test]
async fn test_creator_kick_audits_optional_reason_without_posting_it_publicly() {
    let database = database_with_players(&[(10, "Creator"), (20, "Target")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let lobby_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord lobby message");
    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Add,
            lobby_message_id,
            20,
            "Target",
        ))
        .await
        .expect("target joins");

    dispatch_command(
        &provider,
        "kick",
        10,
        "Creator",
        vec![
            player_option(20, "Target"),
            reason_option("repeated ready-check griefing"),
        ],
    )
    .await;

    let history = ModerationService::new(ModerationRepository::new(database.path()))
        .history(Some(42), Some(20))
        .expect("moderation history");
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].event_type,
        cama_db::moderation::ModerationEventType::Kick
    );
    assert_eq!(history[0].scope, Some(SuspensionScope::Open));
    assert_eq!(history[0].source, ModerationSource::Creator);
    assert_eq!(history[0].actor_id, Some(10));
    assert_eq!(
        history[0].reason.as_deref(),
        Some("repeated ready-check griefing")
    );
    let state = transport.state.lock().expect("transport state");
    assert_eq!(state.direct_messages.len(), 1);
    assert!(
        state.direct_messages[0]
            .1
            .response
            .content
            .contains("repeated ready-check griefing")
    );
    assert!(state.sent.iter().all(|sent| {
        !sent
            .message
            .response
            .content
            .contains("repeated ready-check griefing")
    }));
}

#[tokio::test]
async fn test_admin_kick_clears_both_lobbies_with_a_single_dm() {
    let database = database_with_players(&[(1, "Admin"), (42, "Target"), (99, "Owner")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for_admin(&database, transport.clone(), 1);
    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        create_lobby_and_join_player(&provider, kind, 99, "Owner", 42, "Target").await;
    }

    let responder = dispatch_command(
        &provider,
        "kick",
        1,
        "Admin",
        vec![player_option(42, "Target"), reason_option("queue griefing")],
    )
    .await;

    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        assert!(
            !lobby_snapshot(&provider, kind)
                .players
                .contains(&AppUserId(42))
        );
    }
    let history = ModerationService::new(ModerationRepository::new(database.path()))
        .history(Some(42), Some(42))
        .expect("moderation history");
    assert_eq!(history.len(), 2);
    assert_eq!(
        history.iter().map(|event| event.scope).collect::<Vec<_>>(),
        vec![Some(SuspensionScope::Lowskill), Some(SuspensionScope::Open)]
    );
    assert!(
        history
            .iter()
            .all(|event| event.source == ModerationSource::Admin)
    );
    let state = transport.state.lock().expect("transport state");
    assert_eq!(state.direct_messages.len(), 1);
    let dm = &state.direct_messages[0].1.response.content;
    assert!(dm.contains("queue griefing"));
    assert!(dm.contains(LobbyKind::Open.label()));
    assert!(dm.contains(LobbyKind::LowSkill.label()));
    drop(state);
    let captured = responder.captured.lock().expect("responses");
    assert!(captured.followups.iter().any(|response| {
        response.content.contains(LobbyKind::Open.label())
            && response.content.contains(LobbyKind::LowSkill.label())
    }));
}

#[tokio::test]
async fn test_creator_kick_reaches_only_the_lobby_they_opened() {
    let database = database_with_players(&[(7, "Creator"), (42, "Target"), (99, "Owner")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport);
    create_lobby_and_join_player(&provider, LobbyKind::Open, 7, "Creator", 42, "Target").await;
    create_lobby_and_join_player(&provider, LobbyKind::LowSkill, 99, "Owner", 42, "Target").await;

    let responder = dispatch_command(
        &provider,
        "kick",
        7,
        "Creator",
        vec![player_option(42, "Target")],
    )
    .await;

    assert!(
        !lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(42))
    );
    assert!(
        lobby_snapshot(&provider, LobbyKind::LowSkill)
            .players
            .contains(&AppUserId(42))
    );
    let captured = responder.captured.lock().expect("responses");
    let response = captured.followups.last().expect("kick response");
    assert!(response.content.contains(LobbyKind::Open.label()));
    assert!(!response.content.contains(LobbyKind::LowSkill.label()));
}

#[tokio::test]
async fn test_non_creator_cannot_kick_from_either_lobby() {
    let database = database_with_players(&[(7, "Member"), (42, "Target"), (99, "Owner")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport);
    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        create_lobby_and_join_player(&provider, kind, 99, "Owner", 7, "Member").await;
        let message_id = to_u64(
            lobby_snapshot(&provider, kind)
                .message_ids
                .message_id
                .expect("lobby message")
                .0,
        )
        .expect("Discord message id");
        provider
            .raw_reaction_observer()
            .observe(raw_sword(RawReactionKind::Add, message_id, 42, "Target"))
            .await
            .expect("target joins lobby");
    }

    let responder = dispatch_command(
        &provider,
        "kick",
        7,
        "Member",
        vec![player_option(42, "Target")],
    )
    .await;

    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        assert!(
            lobby_snapshot(&provider, kind)
                .players
                .contains(&AppUserId(42))
        );
    }
    let captured = responder.captured.lock().expect("responses");
    assert!(
        captured.followups.iter().any(|response| {
            response.ephemeral && response.content.contains("Permission denied")
        })
    );
}

#[tokio::test]
async fn live_readycheck_reaches_quorum_once_with_explicit_user_allowlists() {
    let database = database_with_players(&[(10, "Creator"), (20, "Second")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let lobby_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("message id");
    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Add,
            lobby_message_id,
            20,
            "Second",
        ))
        .await
        .expect("second player joins");
    for (user_id, display_name) in [(10, "Creator"), (20, "Second")] {
        transport.set_member(
            42,
            DiscordGuildMemberSnapshot {
                user_id,
                display_name: display_name.to_owned(),
                presence: DiscordPresence::Online,
                in_voice: false,
                deafened: false,
                activities: Vec::new(),
            },
        );
    }

    dispatch_command(
        &provider,
        "readycheck",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let announcement = ready_announcement(LobbyKind::Open, 2);
    let sent = transport.sent_messages();
    assert_eq!(
        sent.iter()
            .filter(|sent| sent.message.response.content == announcement)
            .count(),
        1,
        "one readycheck generation emits the quorum announcement exactly once"
    );
    assert!(sent.iter().any(|sent| {
        sent.channel_id == 9
            && sent.message.response.content == announcement
            && sent
                .message
                .response
                .content
                .ends_with("You can `/shuffle` now; only ready players will be included.")
    }));
    assert!(sent.iter().filter(|sent| sent.message.response.content.contains("<@")) .all(
        |sent| matches!(sent.message.allowed_mentions, DiscordAllowedMentions::Users(ref users) if !users.is_empty())
    ));
}

#[tokio::test]
async fn successful_bell_shortcut_advertises_in_the_persisted_origin_channel() {
    let database = database_with_players(&[(10, "Creator")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    let repository = ReadycheckRepository::new(database.path());
    let mut persisted = repository
        .load(scope.lobby_id(), Some(42))
        .expect("load lobby row")
        .expect("persisted lobby");
    persisted
        .player_join_times
        .insert(10, unix_time_now() - 11.0 * 60.0);
    repository.save(&persisted).expect("age the signup");
    let restarted = provider_for(&database, transport.clone());
    transport.set_member(
        42,
        DiscordGuildMemberSnapshot {
            user_id: 10,
            display_name: "Creator".to_owned(),
            presence: DiscordPresence::Online,
            in_voice: false,
            deafened: false,
            activities: Vec::new(),
        },
    );
    let lobby = lobby_snapshot(&restarted, LobbyKind::Open);
    let message_id = to_u64(lobby.message_ids.message_id.expect("lobby message").0)
        .expect("Discord lobby message");
    let sent_before = transport.sent_messages().len();

    restarted
        .raw_reaction_observer()
        .observe(RawReactionEvent {
            kind: RawReactionKind::Add,
            guild_id: Some(42),
            channel_id: 700,
            message_id,
            user_id: 10,
            actor_is_bot: Some(false),
            actor_display_name: Some("Creator".to_owned()),
            emoji: RawReactionEmoji::unicode(BELL_EMOJI),
        })
        .await
        .expect("bell shortcut");

    let sent = transport.sent_messages();
    assert!(sent[sent_before..].iter().any(|sent| {
        sent.channel_id == 9
            && sent
                .message
                .response
                .content
                .contains("[React in the lobby thread]")
            && sent.message.allowed_mentions == DiscordAllowedMentions::Users(BTreeSet::from([10]))
    }));
}

#[tokio::test]
async fn live_readycheck_reconciles_raw_join_and_leave_against_the_active_generation() {
    let database = database_with_players(&[(10, "Creator"), (20, "Recent Join")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    let lobby_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord lobby message");
    dispatch_command(
        &provider,
        "readycheck",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let before = provider
        .handler
        .state
        .readychecks
        .readycheck_generation(scope)
        .expect("initial readycheck");
    assert_eq!(before.lobby_ids, BTreeSet::from([AppUserId(10)]));

    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Add,
            lobby_message_id,
            20,
            "Recent Join",
        ))
        .await
        .expect("raw join");
    let joined = provider
        .handler
        .state
        .readychecks
        .readycheck_generation(scope)
        .expect("updated readycheck");
    assert_eq!(
        joined.lobby_ids,
        BTreeSet::from([AppUserId(10), AppUserId(20)])
    );
    assert!(joined.reacted.contains_key(&AppUserId(20)));
    assert!(
        transport
            .state
            .lock()
            .expect("transport state")
            .edits
            .iter()
            .any(|(_, message_id, message)| {
                *message_id == to_u64(joined.message_id.0).expect("readycheck message")
                    && message.content_mode
                        == crate::discord_transport::DiscordMessageContentMode::Preserve
            })
    );
    let announcement = ready_announcement(LobbyKind::Open, 2);
    assert_eq!(
        transport
            .sent_messages()
            .iter()
            .filter(|sent| sent.message.response.content == announcement)
            .count(),
        1
    );

    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Remove,
            lobby_message_id,
            20,
            "Recent Join",
        ))
        .await
        .expect("raw leave");
    let left = provider
        .handler
        .state
        .readychecks
        .readycheck_generation(scope)
        .expect("readycheck remains live");
    assert_eq!(left.lobby_ids, BTreeSet::from([AppUserId(10)]));
    assert!(!left.reacted.contains_key(&AppUserId(20)));
    assert!(transport.sent_messages().iter().any(|sent| {
        sent.message.response.content == "🚪 Recent Join left 🍽️ All You Can Feed."
    }));
    assert!(
        transport
            .state
            .lock()
            .expect("transport state")
            .removed_reactions
            .iter()
            .any(|(_, message_id, emoji, user_id)| {
                *message_id == to_u64(left.message_id.0).expect("readycheck message")
                    && emoji.name == READY_EMOJI
                    && *user_id == 20
            })
    );
}

#[tokio::test]
async fn test_join_blocked_during_active_curfew_window() {
    let database = database_with_players(&[(99, "Creator"), (1, "Sleepy")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        99,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let now = chrono::Utc::now();
    let start = now - chrono::Duration::minutes(30);
    let end = now + chrono::Duration::minutes(30);
    CurfewRepository::new(database.path())
        .add_or_replace(&CurfewWindow {
            discord_id: 1,
            guild_id: 42,
            name: "sleep".to_owned(),
            start_hour: start.hour(),
            start_minute: start.minute(),
            end_hour: end.hour(),
            end_minute: end.minute(),
            timezone: Some("UTC".to_owned()),
            days: None,
        })
        .expect("seed an always-active curfew window");

    let slash = dispatch_command(
        &provider,
        "join",
        1,
        "Sleepy",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    {
        let captured = slash.captured.lock().expect("slash responses");
        let response = captured.followups.last().expect("curfew rejection");
        assert!(response.ephemeral);
        assert!(response.content.to_lowercase().contains("sleep"));
    }
    assert!(
        !lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(1))
    );
}

#[tokio::test]
async fn test_join_allowed_outside_curfew_window() {
    let database = database_with_players(&[(99, "Creator"), (1, "Player")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        99,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    dispatch_command(
        &provider,
        "join",
        1,
        "Player",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    assert!(
        lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(1))
    );
}

#[tokio::test]
async fn test_join_allowed_when_curfew_service_unwired() {
    // The live runtime always wires a CurfewService, so the closest parity
    // with Python's "service absent" case is a player who has a general
    // timezone on file but no curfew windows: the join path must still
    // resolve the curfew lookup to "nothing active" without erroring.
    let database = database_with_players(&[(99, "Creator"), (1, "Player")]);
    {
        let connection =
            rusqlite::Connection::open(database.path()).expect("open database for seeding");
        connection
            .execute(
                "UPDATE players SET timezone = 'America/New_York' WHERE discord_id = 1",
                [],
            )
            .expect("seed general timezone without any curfew window");
    }
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        99,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    dispatch_command(
        &provider,
        "join",
        1,
        "Player",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    assert!(
        lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(1))
    );
}

#[tokio::test]
async fn test_curfew_sweep_refreshes_the_lobby_display_after_removing_a_player() {
    // Regression test: a curfew kick must not just mutate in-memory lobby
    // state, it must also re-render the lobby's live Discord embed —
    // otherwise players still show up as queued even though they were
    // actually removed. Mirrors Python's
    // `_deliver_curfew_kick` -> `_sync_lobby_displays` pair.
    let database = database_with_players(&[(99, "Creator"), (1, "Sleepy")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        99,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    dispatch_command(
        &provider,
        "join",
        1,
        "Sleepy",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    assert!(
        lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(1))
    );

    let now = chrono::Utc::now();
    let start = now - chrono::Duration::minutes(30);
    let end = now + chrono::Duration::minutes(30);
    CurfewRepository::new(database.path())
        .add_or_replace(&CurfewWindow {
            discord_id: 1,
            guild_id: 42,
            name: "sleep".to_owned(),
            start_hour: start.hour(),
            start_minute: start.minute(),
            end_hour: end.hour(),
            end_minute: end.minute(),
            timezone: Some("UTC".to_owned()),
            days: None,
        })
        .expect("seed an always-active curfew window");

    let edits_before = transport.edit_count();
    let lobby = provider.live_lobby_service();
    let kicks = provider.curfew_service().sweep(&lobby, &[42], now);
    assert_eq!(kicks.len(), 1);
    for kick in &kicks {
        provider
            .curfew_lobby_display()
            .refresh_curfew_lobby(kick.guild_id, kick.lobby_kind)
            .await
            .expect("refresh lobby display after curfew kick");
    }

    assert!(
        !lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(1))
    );
    assert!(
        transport.edit_count() > edits_before,
        "the lobby's Discord message must be edited after a curfew kick, not just mutated in memory"
    );
}

#[tokio::test]
async fn test_curfew_sweep_removes_the_kicked_players_sword_reaction() {
    // Regression test: a curfew kick must also strip the removed player's
    // own sword reaction from the lobby message — otherwise the reaction
    // still implies they're queued even though the embed and roster agree
    // they're gone.
    let database = database_with_players(&[(99, "Creator"), (1, "Sleepy")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        99,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let lobby_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord message id");
    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Add,
            lobby_message_id,
            1,
            "Sleepy",
        ))
        .await
        .expect("player joins via sword reaction");
    assert!(
        lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(1))
    );

    let now = chrono::Utc::now();
    let start = now - chrono::Duration::minutes(30);
    let end = now + chrono::Duration::minutes(30);
    CurfewRepository::new(database.path())
        .add_or_replace(&CurfewWindow {
            discord_id: 1,
            guild_id: 42,
            name: "sleep".to_owned(),
            start_hour: start.hour(),
            start_minute: start.minute(),
            end_hour: end.hour(),
            end_minute: end.minute(),
            timezone: Some("UTC".to_owned()),
            days: None,
        })
        .expect("seed an always-active curfew window");

    let lobby = provider.live_lobby_service();
    let kicks = provider.curfew_service().sweep(&lobby, &[42], now);
    assert_eq!(kicks.len(), 1);
    for kick in &kicks {
        provider
            .curfew_lobby_display()
            .remove_curfew_lobby_reaction(kick.guild_id, kick.lobby_kind, kick.discord_id)
            .await
            .expect("remove sword reaction after curfew kick");
    }

    let state = transport.state.lock().expect("transport state");
    assert!(
        state
            .removed_reactions
            .iter()
            .any(|(_, message_id, emoji, user_id)| {
                *message_id == lobby_message_id && emoji.name == SWORD_EMOJI && *user_id == 1
            }),
        "curfew kick must remove the kicked player's own sword reaction"
    );
}

#[tokio::test]
async fn test_auto_join_blocked_during_active_curfew_window() {
    let database = database_with_players(&[(99, "Creator"), (1, "Sleepy")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        99,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let now = chrono::Utc::now();
    let start = now - chrono::Duration::minutes(30);
    let end = now + chrono::Duration::minutes(30);
    CurfewRepository::new(database.path())
        .add_or_replace(&CurfewWindow {
            discord_id: 1,
            guild_id: 42,
            name: "sleep".to_owned(),
            start_hour: start.hour(),
            start_minute: start.minute(),
            end_hour: end.hour(),
            end_minute: end.minute(),
            timezone: Some("UTC".to_owned()),
            days: None,
        })
        .expect("seed an always-active curfew window");

    // `/lobby` on an already-created lobby takes the auto-join path
    // (`join_registered_player`), not the explicit `/join` command's own
    // curfew check — this must be blocked too.
    let slash = dispatch_command(
        &provider,
        "lobby",
        1,
        "Sleepy",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    {
        let captured = slash.captured.lock().expect("slash responses");
        let response = captured.followups.last().expect("curfew rejection");
        assert!(response.content.to_lowercase().contains("sleep"));
    }
    assert!(
        !lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(1))
    );
}

#[tokio::test]
async fn test_lobby_creation_blocked_during_active_curfew_window() {
    let database = database_with_players(&[(1, "Sleepy")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());

    let now = chrono::Utc::now();
    let start = now - chrono::Duration::minutes(30);
    let end = now + chrono::Duration::minutes(30);
    CurfewRepository::new(database.path())
        .add_or_replace(&CurfewWindow {
            discord_id: 1,
            guild_id: 42,
            name: "sleep".to_owned(),
            start_hour: start.hour(),
            start_minute: start.minute(),
            end_hour: end.hour(),
            end_minute: end.minute(),
            timezone: Some("UTC".to_owned()),
            days: None,
        })
        .expect("seed an always-active curfew window");

    // Opening a lobby is participation, so curfew blocks it outright rather
    // than publishing a lobby the creator is immediately refused entry to.
    let slash = dispatch_command(
        &provider,
        "lobby",
        1,
        "Sleepy",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    {
        let captured = slash.captured.lock().expect("slash responses");
        let response = captured.followups.last().expect("curfew rejection");
        assert!(
            response.content.contains("can't open a lobby"),
            "unexpected rejection: {}",
            response.content
        );
        assert!(response.content.to_lowercase().contains("sleep"));
    }
    // No lobby was published, and nothing was posted to the channel.
    assert!(
        provider
            .handler
            .state
            .service
            .get_lobby(LobbyScope::new(AppGuildId(42), LobbyKind::Open))
            .is_none()
    );
    assert!(
        transport
            .state
            .lock()
            .expect("transport state")
            .sent
            .is_empty()
    );
}

#[tokio::test]
async fn test_sword_reaction_join_blocked_during_active_curfew_window() {
    let database = database_with_players(&[(99, "Creator"), (1, "Sleepy")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        99,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let lobby_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord message id");

    let now = chrono::Utc::now();
    let start = now - chrono::Duration::minutes(30);
    let end = now + chrono::Duration::minutes(30);
    CurfewRepository::new(database.path())
        .add_or_replace(&CurfewWindow {
            discord_id: 1,
            guild_id: 42,
            name: "sleep".to_owned(),
            start_hour: start.hour(),
            start_minute: start.minute(),
            end_hour: end.hour(),
            end_minute: end.minute(),
            timezone: Some("UTC".to_owned()),
            days: None,
        })
        .expect("seed an always-active curfew window");

    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Add,
            lobby_message_id,
            1,
            "Sleepy",
        ))
        .await
        .expect("raw curfew rejection");

    assert!(
        !lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(1))
    );
    let state = transport.state.lock().expect("transport state");
    assert!(
        state
            .removed_reactions
            .iter()
            .any(|(_, message_id, emoji, user_id)| {
                *message_id == lobby_message_id && emoji.name == SWORD_EMOJI && *user_id == 1
            })
    );
    // The window's name, times, and timezone are private. The public channel
    // message must stay generic; the specifics go out by DM.
    let public = &state.sent.last().expect("public curfew rejection").message;
    assert_eq!(
        public.response.content,
        "<@1> ❌ You're inside one of your curfew windows. Check your DMs, or use `/player curfew list`."
    );
    let lowered = public.response.content.to_lowercase();
    assert!(!lowered.contains("sleep"), "window name leaked publicly");
    assert!(!lowered.contains("utc"), "window timezone leaked publicly");

    let (recipient, direct) = state.direct_messages.last().expect("curfew DM");
    assert_eq!(*recipient, 1);
    assert!(
        direct.response.content.contains("\"sleep\""),
        "the DM should name the window: {}",
        direct.response.content
    );
    assert!(direct.response.content.contains("/player curfew remove"));
}
