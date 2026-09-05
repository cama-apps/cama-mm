use super::*;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::discord_transport::{
    DiscordGuildMemberSnapshot, DiscordMessageReceipt, DiscordMessageSnapshot, DiscordUserSnapshot,
};
use crate::gateway_events::{GatewayMember, GuildMemberPageSource};
use crate::push_notification_provider::{PushNotificationRegistrationProvider, PushPublisher};
use crate::raw_reactions::RawReactionEmoji;
use crate::registration::{InteractionAllowedMentions, InteractionResponseError, Registry};
use crate::test_support::initialize_test_database as initialize_or_migrate;
use cama_app::moderation::{CreateSuspension, ModerationService};
use cama_db::core_repositories::NewPlayer;
use cama_db::low_priority_repository::{LowPriorityRepository, SetLowPriorityInput};
use cama_db::moderation::{
    ModerationRepository, ModerationSource, SuspensionCompletion, SuspensionScope,
};
use cama_db::push_notifications::PushNotificationRepository;
use cama_domain::curfew::CurfewWindow;
use chrono::Timelike;
use tempfile::NamedTempFile;

#[derive(Default)]
struct CapturedResponses {
    deferred: Vec<bool>,
    followups: Vec<InteractionResponse>,
}

#[derive(Default)]
struct RecordingPushPublisher {
    titles: Mutex<Vec<String>>,
}

impl RecordingPushPublisher {
    async fn wait_for_title(&self, expected: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if self
                .titles
                .lock()
                .expect("push titles")
                .iter()
                .any(|title| title == expected)
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[async_trait]
impl PushPublisher for RecordingPushPublisher {
    async fn publish(&self, _topic: &str, title: &str, _message: &str) -> Result<(), String> {
        self.titles
            .lock()
            .expect("push titles")
            .push(title.to_owned());
        Ok(())
    }
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
    /// Threads currently archived. Discord rejects `add_thread_member` on an
    /// archived thread, while sending a message auto-unarchives it; mirror
    /// both so tests can assert join-publication ordering.
    archived_threads: BTreeSet<u64>,
    unpinned: Vec<(u64, u64)>,
    removed_reactions: Vec<(u64, u64, DiscordEmoji, u64)>,
    cleared_reactions: Vec<(u64, u64, DiscordEmoji)>,
    direct_messages: Vec<(u64, DiscordMessage)>,
    members: BTreeMap<(u64, u64), DiscordGuildMemberSnapshot>,
    server_nicknames: BTreeMap<(u64, u64), Option<String>>,
    users: BTreeMap<u64, DiscordUserSnapshot>,
    /// Bot-authored messages retrievable by delivery nonce, mirroring
    /// `find_message_by_delivery_key` history recovery.
    delivery_keys: BTreeMap<(u64, String), DiscordMessageReceipt>,
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
            archived_threads: BTreeSet::new(),
            unpinned: Vec::new(),
            removed_reactions: Vec::new(),
            cleared_reactions: Vec::new(),
            direct_messages: Vec::new(),
            members: BTreeMap::new(),
            server_nicknames: BTreeMap::new(),
            users: BTreeMap::new(),
            delivery_keys: BTreeMap::new(),
        }
    }
}

#[derive(Default)]
struct RecordingTransport {
    state: Mutex<RecordingState>,
    /// When set, message edits fail, standing in for a deleted lobby message or
    /// a transient Discord error during a repaint.
    fail_edits: std::sync::atomic::AtomicBool,
    fail_next_pruned_notice: std::sync::atomic::AtomicBool,
}

impl RecordingTransport {
    fn fail_edits(&self) {
        self.fail_edits
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn fail_next_pruned_notice(&self) {
        self.fail_next_pruned_notice
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

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
            .server_nicknames
            .insert(
                (guild_id, member.user_id),
                Some(member.display_name.clone()),
            );
        self.set_member_without_nickname(guild_id, member);
    }

    fn set_member_without_nickname(&self, guild_id: u64, member: DiscordGuildMemberSnapshot) {
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

    /// Seed channel history with a bot message already carrying
    /// `delivery_key`, standing in for a send whose acknowledgement was lost
    /// before a crash.
    fn seed_delivery_key(&self, channel_id: u64, delivery_key: &str) {
        self.state
            .lock()
            .expect("transport state")
            .delivery_keys
            .insert(
                (channel_id, delivery_key.to_owned()),
                DiscordMessageReceipt {
                    channel_id,
                    message_id: 999,
                    jump_url: format!("https://discord.com/channels/42/{channel_id}/999"),
                },
            );
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
        if message
            .response
            .content
            .starts_with("🧹 Removed (away during ready check):")
            && self
                .fail_next_pruned_notice
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err("ready-check removal notice refused".to_owned());
        }
        let mut state = self.state.lock().expect("transport state");
        state.archived_threads.remove(&channel_id);
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

    async fn send_message_with_delivery_key(
        &self,
        channel_id: u64,
        delivery_key: &str,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        let receipt = self.send_message(channel_id, message).await?;
        self.state
            .lock()
            .expect("transport state")
            .delivery_keys
            .insert((channel_id, delivery_key.to_owned()), receipt.clone());
        Ok(receipt)
    }

    async fn find_message_by_delivery_key(
        &self,
        channel_id: u64,
        delivery_key: &str,
        _after_unix_seconds: i64,
        _limit: usize,
    ) -> Result<Option<DiscordMessageReceipt>, String> {
        Ok(self
            .state
            .lock()
            .expect("transport state")
            .delivery_keys
            .get(&(channel_id, delivery_key.to_owned()))
            .cloned())
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        if self.fail_edits.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("message edit refused".to_owned());
        }
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
        let mut state = self.state.lock().expect("transport state");
        if state.archived_threads.contains(&thread_id) {
            return Err("cannot add a member to an archived thread".to_owned());
        }
        state.thread_members.push((thread_id, member_id));
        Ok(())
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(&self, thread_id: u64, name: &str, locked: bool) -> Result<(), String> {
        let mut state = self.state.lock().expect("transport state");
        state.archived_threads.insert(thread_id);
        state.archived.push((thread_id, name.to_owned(), locked));
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

    fn cached_guild_member_render_names(
        &self,
        guild_id: u64,
        user_ids: &[u64],
    ) -> Result<Option<BTreeMap<u64, String>>, String> {
        let state = self.state.lock().expect("transport state");
        Ok(Some(
            state
                .members
                .keys()
                .filter(|(member_guild_id, user_id)| {
                    *member_guild_id == guild_id && user_ids.contains(user_id)
                })
                .map(|(_, user_id)| {
                    let name = state
                        .server_nicknames
                        .get(&(guild_id, *user_id))
                        .cloned()
                        .flatten()
                        .or_else(|| {
                            state
                                .members
                                .get(&(guild_id, *user_id))
                                .map(|member| member.display_name.clone())
                        })
                        .unwrap_or_else(|| user_id.to_string());
                    (*user_id, name)
                })
                .collect(),
        ))
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
    initialize_or_migrate(database.path()).expect("initialize Python-compatible schema");
    let provider = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::new(),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 100,
            min_readycheck_players: 0,
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
        min_readycheck_players: 0,
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

/// Dispatch that tolerates a handler error, for failure paths whose recovery
/// behavior is the thing under test.
async fn dispatch_command_allowing_failure(
    provider: &LobbyRegistrationProvider,
    name: &str,
    user_id: u64,
    display_name: &str,
    options: Vec<InteractionOption>,
) -> Arc<CapturingResponder> {
    let responder = Arc::new(CapturingResponder::default());
    let _ = registry_for(provider)
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
        .await;
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
async fn slash_join_posts_the_same_thread_line_as_a_sword_react() {
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
    // /join still gets its private ephemeral confirmation, but now also
    // posts the same ping-suppressed "joined." mention into the thread as
    // /lobby and a sword react do -- that mention is what silently
    // subscribes the joiner to the thread, so no explicit thread-member
    // call is needed for any join path any more.
    let sent = transport.sent_messages();
    assert_eq!(sent.len(), messages_before_join + 1);
    let join_message = &sent[messages_before_join].message;
    assert_eq!(join_message.response.content, "✅ <@20> joined.");
    assert_eq!(join_message.allowed_mentions, DiscordAllowedMentions::None);
    assert!(
        transport
            .state
            .lock()
            .expect("transport state")
            .thread_members
            .is_empty()
    );
    assert_eq!(observer.confirmed.lock().expect("join observer").len(), 2);
}

#[tokio::test]
async fn join_during_an_archived_thread_spell_still_subscribes_the_joiner() {
    let database = database_with_players(&[(10, "Creator"), (20, "Joiner")]);
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
    let thread_id =
        to_u64(lobby.message_ids.thread_id.expect("lobby thread").0).expect("Discord thread id");
    let message_id =
        to_u64(lobby.message_ids.message_id.expect("lobby message").0).expect("Discord message id");
    transport
        .archive_thread(thread_id, "🍽️ All You Can Feed", false)
        .await
        .expect("archive lobby thread");

    provider
        .raw_reaction_observer()
        .observe(raw_sword(RawReactionKind::Add, message_id, 20, "Joiner"))
        .await
        .expect("raw sword join");

    // The ping-suppressed @mention is both what auto-unarchives the thread
    // and what subscribes the joiner to it (Discord treats a mention as an
    // organic join), so it must still be posted into the archived thread
    // and no explicit thread-member call may be attempted -- that API is
    // rejected on an archived thread and would also print its own "X added
    // Y to the thread" system line.
    // /lobby already posted the creator's own "<@10> joined." line before
    // the archive, so look specifically for the sword joiner's.
    let join_lines = transport
        .sent_messages()
        .into_iter()
        .filter(|sent| {
            sent.channel_id == thread_id && sent.message.response.content == "✅ <@20> joined."
        })
        .collect::<Vec<_>>();
    assert_eq!(
        join_lines.len(),
        1,
        "the joiner's announcement must reach the archived thread exactly once"
    );
    assert_eq!(
        join_lines[0].message.allowed_mentions,
        DiscordAllowedMentions::None
    );
    let state = transport.state.lock().expect("transport state");
    assert!(
        !state.archived_threads.contains(&thread_id),
        "posting the join line must auto-unarchive the thread"
    );
    assert!(
        state.thread_members.is_empty(),
        "subscription rides on the mention; no explicit thread-member call may be made"
    );
}

#[tokio::test]
async fn slash_and_raw_membership_changes_share_one_churn_cooldown() {
    let database = database_with_players(&[(10, "Creator"), (20, "Churner")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport);
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord message id");
    let scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    assert!(
        provider
            .handler
            .state
            .service
            .join_lobby(AppUserId(20), scope)
            .success
    );

    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Remove,
            message_id,
            20,
            "Churner",
        ))
        .await
        .expect("raw sword leave");
    let joined = dispatch_command(
        &provider,
        "join",
        20,
        "Churner",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    assert_eq!(
        joined.captured.lock().expect("responses").followups[0].content,
        "✅ Joined 🍽️ All You Can Feed!"
    );
    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Remove,
            message_id,
            20,
            "Churner",
        ))
        .await
        .expect("raw sword leave");

    let blocked = dispatch_command(
        &provider,
        "join",
        20,
        "Churner",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let blocked_content = &blocked.captured.lock().expect("responses").followups[0].content;
    assert!(blocked_content.starts_with("Slow down!"));
    assert!(blocked_content.contains("joining or leaving again in"));
    assert!(
        !lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(20)),
        "a blocked join must not change lobby membership"
    );
}

#[tokio::test]
async fn churn_cooldown_is_shared_across_lobby_kinds_and_preserves_blocked_leave() {
    let database = database_with_players(&[(10, "Creator"), (20, "Churner")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport);
    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        dispatch_command(&provider, "lobby", 10, "Creator", vec![lobby_option(kind)]).await;
    }
    let open_scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    assert!(
        provider
            .handler
            .state
            .service
            .join_lobby(AppUserId(20), open_scope)
            .success
    );
    let open_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("open lobby message")
            .0,
    )
    .expect("Discord message id");

    dispatch_command(
        &provider,
        "join",
        20,
        "Churner",
        vec![lobby_option(LobbyKind::LowSkill)],
    )
    .await;
    for kind in [RawReactionKind::Remove, RawReactionKind::Add] {
        provider
            .raw_reaction_observer()
            .observe(raw_sword(kind, open_message_id, 20, "Churner"))
            .await
            .expect("raw sword membership change");
    }

    let blocked = dispatch_command(&provider, "leave", 20, "Churner", Vec::new()).await;
    assert!(
        blocked.captured.lock().expect("responses").followups[0]
            .content
            .starts_with("Slow down!")
    );
    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        assert!(
            lobby_snapshot(&provider, kind)
                .players
                .contains(&AppUserId(20)),
            "a blocked leave must preserve every queued lobby"
        );
    }
}

#[tokio::test]
async fn raw_sword_churn_rejection_is_visible_and_does_not_rejoin() {
    let database = database_with_players(&[(10, "Creator"), (20, "Churner")]);
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
    let message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord message id");
    let scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    assert!(
        provider
            .handler
            .state
            .service
            .join_lobby(AppUserId(20), scope)
            .success
    );

    for kind in [
        RawReactionKind::Remove,
        RawReactionKind::Add,
        RawReactionKind::Remove,
    ] {
        provider
            .raw_reaction_observer()
            .observe(raw_sword(kind, message_id, 20, "Churner"))
            .await
            .expect("raw sword membership change");
    }
    provider
        .raw_reaction_observer()
        .observe(raw_sword(RawReactionKind::Add, message_id, 20, "Churner"))
        .await
        .expect("rate-limited raw sword join");

    assert!(
        !lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(20))
    );
    assert!(transport.sent_messages().iter().any(|sent| {
        sent.message
            .response
            .content
            .starts_with("<@20> ❌ Slow down!")
    }));
}

#[tokio::test]
async fn rate_limited_missing_lobby_does_not_create_an_empty_lobby() {
    let database = database_with_players(&[(10, "Creator"), (20, "Churner")]);
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

    dispatch_command(
        &provider,
        "join",
        20,
        "Churner",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    dispatch_command(&provider, "leave", 20, "Churner", Vec::new()).await;
    dispatch_command(
        &provider,
        "join",
        20,
        "Churner",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let threads_before = transport.thread_count();
    let blocked = dispatch_command(
        &provider,
        "lobby",
        20,
        "Churner",
        vec![lobby_option(LobbyKind::LowSkill)],
    )
    .await;
    let response = blocked.captured.lock().expect("responses");
    assert!(response.followups[0].content.starts_with("Slow down!"));
    assert!(!response.followups[0].content.contains("created"));
    assert!(
        provider
            .handler
            .state
            .service
            .get_lobby(LobbyScope::new(AppGuildId(42), LobbyKind::LowSkill))
            .is_none()
    );
    assert_eq!(transport.thread_count(), threads_before);
}

#[tokio::test]
async fn blocked_raw_leave_keeps_membership_and_duplicate_add_repairs_the_retry_path() {
    let database = database_with_players(&[(10, "Creator"), (20, "Churner")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
        dispatch_command(&provider, "lobby", 10, "Creator", vec![lobby_option(kind)]).await;
    }
    let scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    let message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord message id");
    assert!(
        provider
            .handler
            .state
            .service
            .join_lobby(AppUserId(20), scope)
            .success
    );

    for kind in [RawReactionKind::Remove, RawReactionKind::Add] {
        provider
            .raw_reaction_observer()
            .observe(raw_sword(kind, message_id, 20, "Churner"))
            .await
            .expect("raw sword membership change");
    }
    dispatch_command(
        &provider,
        "join",
        20,
        "Churner",
        vec![lobby_option(LobbyKind::LowSkill)],
    )
    .await;
    let removed_reactions_before = transport
        .state
        .lock()
        .expect("transport state")
        .removed_reactions
        .len();
    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Remove,
            message_id,
            20,
            "Churner",
        ))
        .await
        .expect("rate-limited raw sword leave");

    assert!(
        lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(20)),
        "a blocked raw leave must preserve membership"
    );
    assert!(transport.sent_messages().iter().any(|sent| {
        sent.message
            .response
            .content
            .starts_with("<@20> ❌ Slow down!")
    }));
    assert_eq!(
        transport
            .state
            .lock()
            .expect("transport state")
            .removed_reactions
            .len(),
        removed_reactions_before,
        "a blocked remove must not remove a reaction that may have been re-added"
    );

    *provider
        .handler
        .state
        .membership_rate_limiter
        .lock()
        .expect("membership limiter") = RateLimiter::new();
    provider
        .raw_reaction_observer()
        .observe(raw_sword(RawReactionKind::Add, message_id, 20, "Churner"))
        .await
        .expect("idempotent raw sword repair");
    provider
        .raw_reaction_observer()
        .observe(raw_sword(
            RawReactionKind::Remove,
            message_id,
            20,
            "Churner",
        ))
        .await
        .expect("raw sword leave after cooldown");
    assert!(
        !lobby_snapshot(&provider, LobbyKind::Open)
            .players
            .contains(&AppUserId(20))
    );
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
            match sent.message.allowed_mentions {
                DiscordAllowedMentions::Users(ref users) => {
                    assert!(!users.is_empty());
                    assert!(users.iter().all(|user_id| [10, 20].contains(user_id)));
                }
                DiscordAllowedMentions::None => {}
                DiscordAllowedMentions::Default => {
                    panic!(
                        "message containing a user tag must carry an explicit user allowlist or suppress mentions entirely"
                    );
                }
            }
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
        account_username: "fetched-spectator-account".to_owned(),
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
    assert!(sent.iter().filter(|sent| sent.message.response.content.contains("<@")).all(
        |sent| matches!(sent.message.allowed_mentions, DiscordAllowedMentions::Users(ref users) if !users.is_empty())
            || sent.message.allowed_mentions == DiscordAllowedMentions::None
    ));
}

#[tokio::test]
async fn readycheck_below_minimum_player_count_is_rejected() {
    let database = database_with_players(&[(10, "Creator"), (20, "Second")]);
    let transport = Arc::new(RecordingTransport::default());
    let mut config = runtime_config();
    config.min_readycheck_players = MINIMUM_READYCHECK_PLAYERS;
    let provider = LobbyRegistrationProvider::new(
        database.path(),
        config,
        Arc::new(DraftStateManager::default()),
        transport.clone(),
    )
    .expect("construct lobby provider with a minimum ready-check floor");
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    dispatch_command(
        &provider,
        "join",
        20,
        "Second",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let messages_before_readycheck = transport.sent_messages().len();

    let responder = dispatch_command(
        &provider,
        "readycheck",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let captured = responder.captured.lock().expect("responses");
    assert_eq!(captured.followups.len(), 1);
    assert_eq!(
        captured.followups[0].content,
        format!(
            "❌ Need at least {MINIMUM_READYCHECK_PLAYERS} players to ready check — you have 2."
        )
    );
    assert!(captured.followups[0].ephemeral);
    assert_eq!(
        transport.sent_messages().len(),
        messages_before_readycheck,
        "a rejected ready check must not post anything into the lobby thread"
    );
}

#[tokio::test]
async fn readycheck_explains_status_join_age_and_automatic_confirmations() {
    let database = database_with_players(&[
        (10, "Creator"),
        (20, "Voice"),
        (30, "Dota"),
        (40, "Away"),
        (50, "Recent"),
        (60, "Legacy"),
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
    let scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    let lobby_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord lobby message");
    for (player_id, name) in [
        (20, "Voice"),
        (30, "Dota"),
        (40, "Away"),
        (50, "Recent"),
        (60, "Legacy"),
    ] {
        provider
            .raw_reaction_observer()
            .observe(raw_sword(
                RawReactionKind::Add,
                lobby_message_id,
                player_id,
                name,
            ))
            .await
            .expect("player joins lobby");
    }

    let repository = ReadycheckRepository::new(database.path());
    let mut persisted = repository
        .load(scope.lobby_id(), Some(42))
        .expect("load lobby row")
        .expect("persisted lobby");
    let now = unix_time_now();
    persisted.player_join_times = BTreeMap::from([
        (10, now - 3_605.0),
        (20, now - 1_205.0),
        (30, now - 1_205.0),
        (40, now - 1_205.0),
        (50, now - 305.0),
    ]);
    repository.save(&persisted).expect("age lobby signups");

    let restarted = provider_for(&database, transport.clone());
    for member in [
        DiscordGuildMemberSnapshot {
            user_id: 10,
            display_name: "Creator".to_owned(),
            presence: DiscordPresence::Online,
            in_voice: false,
            deafened: false,
            activities: Vec::new(),
        },
        DiscordGuildMemberSnapshot {
            user_id: 20,
            display_name: "Voice".to_owned(),
            presence: DiscordPresence::Offline,
            in_voice: true,
            deafened: true,
            activities: Vec::new(),
        },
        DiscordGuildMemberSnapshot {
            user_id: 30,
            display_name: "Dota".to_owned(),
            presence: DiscordPresence::Idle,
            in_voice: false,
            deafened: false,
            activities: vec!["Dota 2".to_owned()],
        },
        DiscordGuildMemberSnapshot {
            user_id: 40,
            display_name: "Away".to_owned(),
            presence: DiscordPresence::Offline,
            in_voice: false,
            deafened: false,
            activities: Vec::new(),
        },
        DiscordGuildMemberSnapshot {
            user_id: 50,
            display_name: "Recent".to_owned(),
            presence: DiscordPresence::Offline,
            in_voice: false,
            deafened: false,
            activities: Vec::new(),
        },
        DiscordGuildMemberSnapshot {
            user_id: 60,
            display_name: "Legacy".to_owned(),
            presence: DiscordPresence::Online,
            in_voice: false,
            deafened: false,
            activities: Vec::new(),
        },
    ] {
        transport.set_member(42, member);
    }
    let sent_before = transport.sent_messages().len();

    dispatch_command(
        &restarted,
        "readycheck",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let sent = transport.sent_messages();
    let observed = sent[sent_before..]
        .iter()
        .map(|sent| {
            (
                sent.channel_id,
                sent.message.response.content.clone(),
                sent.message
                    .response
                    .embeds
                    .iter()
                    .filter_map(|embed| embed.title.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let embed = sent[sent_before..]
        .iter()
        .flat_map(|sent| &sent.message.response.embeds)
        .find(|embed| {
            embed
                .title
                .as_deref()
                .is_some_and(|title| title.ends_with("All You Can Feed Ready Check"))
        })
        .unwrap_or_else(|| panic!("missing readycheck embed in: {observed:?}"));
    let field = |name: &str| {
        embed
            .fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("missing readycheck field {name:?}: {:?}", embed.fields))
    };
    assert_eq!(
        field("✅ Likely Active (3)").value,
        "Voice 🔇🔴 (joined 20m ago)\nDota 🎮🟡 (joined 20m ago)\nLegacy 🟢 (join time unknown)"
    );
    assert_eq!(
        field("⚠️ Possibly AFK (1)").value,
        "<@40> 🔴 (joined 20m ago)"
    );
    assert_eq!(
        field("✅ Confirmed Ready (2)").value,
        "<@10> 🟢 (joined 1h ago)\n<@50> 🆕🔴 (joined 5m ago)"
    );
}

#[tokio::test]
async fn stale_readycheck_publicly_names_pruned_players_in_the_lobby_thread() {
    let database = database_with_players(&[(10, "Creator"), (20, "Away One"), (30, "Away Two")]);
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
    for (player_id, name) in [(20, "Away One"), (30, "Away Two")] {
        provider
            .raw_reaction_observer()
            .observe(raw_sword(
                RawReactionKind::Add,
                lobby_message_id,
                player_id,
                name,
            ))
            .await
            .expect("player joins lobby");
    }

    let repository = ReadycheckRepository::new(database.path());
    let mut persisted = repository
        .load(scope.lobby_id(), Some(42))
        .expect("load lobby row")
        .expect("persisted lobby");
    let long_ago = unix_time_now() - 3_605.0;
    persisted.player_join_times = BTreeMap::from([(10, long_ago), (20, long_ago), (30, long_ago)]);
    repository.save(&persisted).expect("age lobby signups");

    let restarted = provider_for(&database, transport.clone());
    for (user_id, display_name, presence) in [
        (10, "Creator", DiscordPresence::Online),
        (20, "Away One", DiscordPresence::Offline),
        (30, "Away Two", DiscordPresence::Offline),
    ] {
        transport.set_member(
            42,
            DiscordGuildMemberSnapshot {
                user_id,
                display_name: display_name.to_owned(),
                presence,
                in_voice: false,
                deafened: false,
                activities: Vec::new(),
            },
        );
    }
    restarted.handler.state.readychecks.set_readycheck_state(
        scope,
        cama_app::readycheck::ReadycheckStateInput {
            message_id: AppMessageId(5_000),
            channel_id: AppChannelId(9),
            lobby_ids: BTreeSet::from([AppUserId(10), AppUserId(20), AppUserId(30)]),
            player_data: BTreeMap::new(),
            created_at: Some(long_ago),
            initial_reacted: BTreeMap::new(),
        },
    );
    let thread_id = to_u64(
        lobby_snapshot(&restarted, LobbyKind::Open)
            .message_ids
            .thread_id
            .expect("lobby thread")
            .0,
    )
    .expect("Discord lobby thread");
    let sent_before = transport.sent_messages().len();

    dispatch_command(
        &restarted,
        "readycheck",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let sent = transport.sent_messages();
    let notice = sent[sent_before..]
        .iter()
        .find(|sent| {
            sent.message
                .response
                .content
                .starts_with("🧹 Removed (away during ready check):")
        })
        .expect("public stale-sweep notice");
    assert_eq!(notice.channel_id, thread_id);
    assert_eq!(
        notice.message.response.content,
        "🧹 Removed (away during ready check): <@20> <@30> — rejoin All You Can Feed with `/join` if you're back."
    );
    assert_eq!(
        notice.message.allowed_mentions,
        DiscordAllowedMentions::Users(BTreeSet::from([20, 30]))
    );
    let replacement_embed = sent[sent_before..]
        .iter()
        .flat_map(|sent| &sent.message.response.embeds)
        .find(|embed| {
            embed
                .title
                .as_deref()
                .is_some_and(|title| title.ends_with("All You Can Feed Ready Check"))
        })
        .expect("replacement ready-check embed");
    assert!(
        replacement_embed
            .fields
            .iter()
            .all(|field| !field.value.contains("<@20>") && !field.value.contains("<@30>")),
        "players removed by the sweep must not remain in the replacement embed: {:?}",
        replacement_embed.fields
    );
    assert!(
        transport
            .state
            .lock()
            .expect("transport state")
            .direct_messages
            .is_empty(),
        "stale-sweep restoration is public-only"
    );
}

async fn stale_readycheck_fixture(
    database: &NamedTempFile,
    transport: Arc<RecordingTransport>,
) -> LobbyRegistrationProvider {
    let provider = provider_for(database, transport.clone());
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
    for (player_id, name) in [(20, "Away One"), (30, "Away Two")] {
        provider
            .raw_reaction_observer()
            .observe(raw_sword(
                RawReactionKind::Add,
                lobby_message_id,
                player_id,
                name,
            ))
            .await
            .expect("player joins lobby");
    }

    let repository = ReadycheckRepository::new(database.path());
    let mut persisted = repository
        .load(scope.lobby_id(), Some(42))
        .expect("load lobby row")
        .expect("persisted lobby");
    let long_ago = unix_time_now() - 3_605.0;
    persisted.player_join_times = BTreeMap::from([(10, long_ago), (20, long_ago), (30, long_ago)]);
    repository.save(&persisted).expect("age lobby signups");

    let restarted = provider_for(database, transport.clone());
    for (user_id, display_name, presence) in [
        (10, "Creator", DiscordPresence::Online),
        (20, "Away One", DiscordPresence::Offline),
        (30, "Away Two", DiscordPresence::Offline),
    ] {
        transport.set_member(
            42,
            DiscordGuildMemberSnapshot {
                user_id,
                display_name: display_name.to_owned(),
                presence,
                in_voice: false,
                deafened: false,
                activities: Vec::new(),
            },
        );
    }
    restarted.handler.state.readychecks.set_readycheck_state(
        scope,
        cama_app::readycheck::ReadycheckStateInput {
            message_id: AppMessageId(5_000),
            channel_id: AppChannelId(9),
            lobby_ids: BTreeSet::from([AppUserId(10), AppUserId(20), AppUserId(30)]),
            player_data: BTreeMap::new(),
            created_at: Some(long_ago),
            initial_reacted: BTreeMap::new(),
        },
    );
    restarted
}

#[tokio::test]
async fn failed_stale_notice_delivery_does_not_block_readycheck_and_recovers_once() {
    let database = database_with_players(&[(10, "Creator"), (20, "Away One"), (30, "Away Two")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = stale_readycheck_fixture(&database, transport.clone()).await;
    transport.fail_next_pruned_notice();
    let sent_before = transport.sent_messages().len();

    let response = dispatch_command(
        &provider,
        "readycheck",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    assert!(
        response
            .captured
            .lock()
            .expect("readycheck responses")
            .followups
            .iter()
            .any(|response| response.content.starts_with("✅")),
        "the durable notice retry must not block the replacement ready check"
    );

    assert_eq!(
        lobby_snapshot(&provider, LobbyKind::Open).players,
        BTreeSet::from([AppUserId(10)]),
        "the failed Discord notice happens after the durable sweep"
    );
    let notice_count = || {
        transport.sent_messages()[sent_before..]
            .iter()
            .filter(|sent| {
                sent.message
                    .response
                    .content
                    .starts_with("🧹 Removed (away during ready check):")
            })
            .count()
    };
    assert_eq!(notice_count(), 0, "the injected notice send should fail");

    drop(provider);
    let restarted = provider_for(&database, transport.clone());
    let report = restarted
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42]),
            Arc::new(NoMembers),
        ))
        .await;
    assert!(report.failures.is_empty(), "recovery failed: {report:?}");
    assert_eq!(
        notice_count(),
        1,
        "restart recovery should deliver the notice"
    );

    let second_report = restarted
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42]),
            Arc::new(NoMembers),
        ))
        .await;
    assert!(
        second_report.failures.is_empty(),
        "second recovery failed: {second_report:?}"
    );
    assert_eq!(notice_count(), 1, "acknowledged notice must not repeat");
}

#[tokio::test]
async fn failed_stale_notice_recovery_is_nonfatal_and_remains_retryable() {
    let database = database_with_players(&[(10, "Creator"), (20, "Removed")]);
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
    let lobby = repository
        .load(scope.lobby_id(), Some(42))
        .expect("load persisted lobby")
        .expect("persisted lobby");
    let thread_id = lobby.thread_id.expect("persisted lobby thread");
    repository
        .save_with_pruned_notice(&lobby, thread_id, &BTreeSet::from([20]))
        .expect("queue stale-removal notice");
    let sent_before = transport.sent_messages().len();
    transport.fail_next_pruned_notice();

    let first_report = provider
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42]),
            Arc::new(NoMembers),
        ))
        .await;

    assert!(
        first_report.failures.is_empty(),
        "a retained notice must not make the runtime unhealthy: {first_report:?}"
    );
    assert_eq!(
        repository
            .pending_pruned_notices()
            .expect("reload retained notice")
            .notices
            .len(),
        1,
        "failed delivery must remain pending"
    );
    let notice_count = || {
        transport.sent_messages()[sent_before..]
            .iter()
            .filter(|sent| {
                sent.message
                    .response
                    .content
                    .starts_with("🧹 Removed (away during ready check):")
            })
            .count()
    };
    assert_eq!(notice_count(), 0, "the injected delivery should fail");

    let retry_report = provider
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42]),
            Arc::new(NoMembers),
        ))
        .await;
    assert!(
        retry_report.failures.is_empty(),
        "notice retry failed: {retry_report:?}"
    );
    assert_eq!(notice_count(), 1, "the retained notice should retry once");
    assert!(
        repository
            .pending_pruned_notices()
            .expect("reload acknowledged notices")
            .notices
            .is_empty(),
        "successful retry must acknowledge the notice"
    );

    let final_report = provider
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42]),
            Arc::new(NoMembers),
        ))
        .await;
    assert!(
        final_report.failures.is_empty(),
        "post-ack recovery failed: {final_report:?}"
    );
    assert_eq!(notice_count(), 1, "acknowledged notice must not repeat");
}

#[tokio::test]
async fn already_delivered_stale_notice_is_acknowledged_without_a_duplicate_ping() {
    let database = database_with_players(&[(10, "Creator"), (20, "Removed")]);
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
    let lobby = repository
        .load(scope.lobby_id(), Some(42))
        .expect("load persisted lobby")
        .expect("persisted lobby");
    let thread_id = lobby.thread_id.expect("persisted lobby thread");
    repository
        .save_with_pruned_notice(&lobby, thread_id, &BTreeSet::from([20]))
        .expect("queue stale-removal notice");
    // The notice message already exists in channel history: the send
    // succeeded on a previous run, but the process crashed before the
    // acknowledgement DELETE, leaving the outbox row behind.
    let notice = repository
        .pending_pruned_notices()
        .expect("reload pending notice")
        .notices
        .pop()
        .expect("queued notice");
    transport.seed_delivery_key(
        u64::try_from(notice.channel_id).expect("notice channel id"),
        notice.delivery_nonce(),
    );
    let sent_before = transport.sent_messages().len();

    let report = provider
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42]),
            Arc::new(NoMembers),
        ))
        .await;

    assert!(
        report.failures.is_empty(),
        "recovery of an already-delivered notice failed: {report:?}"
    );
    let duplicate_pings = transport.sent_messages()[sent_before..]
        .iter()
        .filter(|sent| {
            sent.message
                .response
                .content
                .starts_with("🧹 Removed (away during ready check):")
        })
        .count();
    assert_eq!(
        duplicate_pings, 0,
        "an already-delivered notice must not ping the pruned players again"
    );
    assert!(
        repository
            .pending_pruned_notices()
            .expect("reload acknowledged notices")
            .notices
            .is_empty(),
        "the already-delivered notice must still be acknowledged"
    );
}

#[tokio::test]
async fn corrupt_pruned_notice_row_does_not_wedge_recovery_for_valid_notices() {
    let database = database_with_players(&[(10, "Creator"), (20, "Removed")]);
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
    let lobby = repository
        .load(scope.lobby_id(), Some(42))
        .expect("load persisted lobby")
        .expect("persisted lobby");
    let thread_id = lobby.thread_id.expect("persisted lobby thread");
    repository
        .save_with_pruned_notice(&lobby, thread_id, &BTreeSet::from([20]))
        .expect("queue stale-removal notice");
    rusqlite::Connection::open(database.path())
        .expect("open lobby database")
        .execute(
            "INSERT INTO app_kv(guild_id,key,value)
             VALUES (42,'readycheck:pruned_notice:1:0000000000000bad','not-json')",
            [],
        )
        .expect("insert corrupted notice row");
    let sent_before = transport.sent_messages().len();

    let report = provider
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42]),
            Arc::new(NoMembers),
        ))
        .await;

    assert!(
        report.failures.is_empty(),
        "one corrupt notice row must not wedge recovery: {report:?}"
    );
    let delivered = transport.sent_messages()[sent_before..]
        .iter()
        .filter(|sent| {
            sent.message
                .response
                .content
                .starts_with("🧹 Removed (away during ready check):")
        })
        .count();
    assert_eq!(delivered, 1, "the valid notice must still publish");
    let listing = repository.pending_pruned_notices().expect("reload notices");
    assert!(
        listing.notices.is_empty(),
        "the published notice must be acknowledged"
    );
    assert_eq!(
        listing.skipped_keys,
        ["readycheck:pruned_notice:1:0000000000000bad"],
        "the corrupt row is retained for diagnosis"
    );
}

#[tokio::test]
async fn invalid_stale_notice_recovery_remains_fatal() {
    let database = database_with_players(&[(10, "Creator"), (20, "Removed")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport);
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
    let lobby = repository
        .load(scope.lobby_id(), Some(42))
        .expect("load persisted lobby")
        .expect("persisted lobby");
    repository
        .save_with_pruned_notice(&lobby, -1, &BTreeSet::from([20]))
        .expect("queue invalid stale-removal notice");

    let report = provider
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            Arc::<[u64]>::from([42]),
            Arc::new(NoMembers),
        ))
        .await;

    assert_eq!(
        report.failures.len(),
        1,
        "invalid persisted notice: {report:?}"
    );
    assert!(
        report.failures[0]
            .message
            .contains("invalid persisted Discord snowflake -1"),
        "unexpected failure: {report:?}"
    );
    assert_eq!(
        repository
            .pending_pruned_notices()
            .expect("reload invalid notice")
            .notices
            .len(),
        1,
        "invalid notice must remain available for diagnosis"
    );
}

#[tokio::test]
async fn stale_readycheck_does_not_prune_without_the_durable_notice_outbox() {
    let database = database_with_players(&[(10, "Creator"), (20, "Away One"), (30, "Away Two")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = stale_readycheck_fixture(&database, transport).await;
    rusqlite::Connection::open(database.path())
        .expect("open lobby database")
        .execute("DROP TABLE app_kv", [])
        .expect("remove ready-check notice outbox");

    dispatch_command_allowing_failure(
        &provider,
        "readycheck",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let expected = BTreeSet::from([AppUserId(10), AppUserId(20), AppUserId(30)]);
    assert_eq!(
        lobby_snapshot(&provider, LobbyKind::Open).players,
        expected,
        "the in-memory lobby must not publish a sweep that cannot queue its notice"
    );
    drop(provider);
    assert_eq!(
        lobby_snapshot(
            &provider_for(&database, Arc::new(RecordingTransport::default())),
            LobbyKind::Open
        )
        .players,
        expected,
        "the durable lobby must remain unchanged when notice persistence fails"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readycheck_push_notification_is_suppressed_for_a_partial_lobby() {
    let database = database_with_players(&[(10, "Solo")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    let push_publisher = Arc::new(RecordingPushPublisher::default());
    let push_provider = PushNotificationRegistrationProvider::with_test_publisher(
        database.path(),
        transport.clone(),
        push_publisher.clone(),
    );
    PushNotificationRepository::new(database.path())
        .set_target(
            10,
            Some(42),
            "cama-000000000000000000000000000000000000000000000010",
            1,
        )
        .expect("configure push target");
    provider
        .attach_push_notification_hooks(push_provider.hooks())
        .expect("attach push hooks");

    dispatch_command(
        &provider,
        "lobby",
        10,
        "Solo",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    dispatch_command(
        &provider,
        "readycheck",
        10,
        "Solo",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    assert!(
        !push_publisher.wait_for_title("⚔️ Readycheck!").await,
        "a readycheck launched against a partial lobby must not push a notification"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readycheck_push_notification_fires_when_the_lobby_is_full() {
    // Player 20 must be aged out of the "just joined" grace window, otherwise
    // they auto-confirm like the invoker does and never land in `mention_ids`
    // -- the same set the push notification is filtered against.
    let database = database_with_players(&[(10, "First"), (20, "Second")]);
    let transport = Arc::new(RecordingTransport::default());
    let provider = provider_for(&database, transport.clone());
    PushNotificationRepository::new(database.path())
        .set_target(
            20,
            Some(42),
            "cama-000000000000000000000000000000000000000000000020",
            1,
        )
        .expect("configure push target");

    dispatch_command(
        &provider,
        "lobby",
        10,
        "First",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    dispatch_command(
        &provider,
        "join",
        20,
        "Second",
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
        .insert(20, unix_time_now() - 11.0 * 60.0);
    repository.save(&persisted).expect("age the signup");

    // Reload from the aged persistence: the live provider's in-memory lobby
    // state does not observe a direct repository write.
    let restarted = provider_for(&database, transport.clone());
    let push_publisher = Arc::new(RecordingPushPublisher::default());
    let push_provider = PushNotificationRegistrationProvider::with_test_publisher(
        database.path(),
        transport.clone(),
        push_publisher.clone(),
    );
    restarted
        .attach_push_notification_hooks(push_provider.hooks())
        .expect("attach push hooks");
    // Only a resolvable guild member lands in the `mentionable` set that the
    // readycheck's push notification is filtered against.
    transport.set_member(
        42,
        DiscordGuildMemberSnapshot {
            user_id: 20,
            display_name: "Second".to_owned(),
            presence: DiscordPresence::Online,
            in_voice: false,
            deafened: false,
            activities: Vec::new(),
        },
    );

    dispatch_command(
        &restarted,
        "readycheck",
        10,
        "First",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    assert!(
        push_publisher.wait_for_title("⚔️ Readycheck!").await,
        "a readycheck launched against a full lobby must push a notification to subscribers"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readycheck_push_notification_fires_when_a_hydrated_lobby_exceeds_the_threshold() {
    // A persisted lobby can hydrate with more players than the configured
    // threshold after `ready_threshold` is lowered between restarts; the push
    // gate must treat that as a full lobby instead of silently never firing.
    let database = database_with_players(&[(10, "First"), (20, "Second"), (30, "Third")]);
    let transport = Arc::new(RecordingTransport::default());
    let mut wide_config = runtime_config();
    wide_config.ready_threshold = 3;
    let provider = LobbyRegistrationProvider::new(
        database.path(),
        wide_config,
        Arc::new(DraftStateManager::default()),
        transport.clone(),
    )
    .expect("construct wide-threshold lobby provider");

    dispatch_command(
        &provider,
        "lobby",
        10,
        "First",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    for (player_id, name) in [(20, "Second"), (30, "Third")] {
        dispatch_command(
            &provider,
            "join",
            player_id,
            name,
            vec![lobby_option(LobbyKind::Open)],
        )
        .await;
    }

    // Age the signups out of the "just joined" grace window so player 20
    // lands in `mention_ids`, matching the full-lobby push test above.
    let scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    let repository = ReadycheckRepository::new(database.path());
    let mut persisted = repository
        .load(scope.lobby_id(), Some(42))
        .expect("load lobby row")
        .expect("persisted lobby");
    for player_id in [20, 30] {
        persisted
            .player_join_times
            .insert(player_id, unix_time_now() - 11.0 * 60.0);
    }
    repository.save(&persisted).expect("age the signups");
    drop(provider);

    // Restart on the default, lower threshold: three players against two.
    let restarted = provider_for(&database, transport.clone());
    let push_publisher = Arc::new(RecordingPushPublisher::default());
    let push_provider = PushNotificationRegistrationProvider::with_test_publisher(
        database.path(),
        transport.clone(),
        push_publisher.clone(),
    );
    PushNotificationRepository::new(database.path())
        .set_target(
            20,
            Some(42),
            "cama-000000000000000000000000000000000000000000000020",
            1,
        )
        .expect("configure push target");
    restarted
        .attach_push_notification_hooks(push_provider.hooks())
        .expect("attach push hooks");
    transport.set_member(
        42,
        DiscordGuildMemberSnapshot {
            user_id: 20,
            display_name: "Second".to_owned(),
            presence: DiscordPresence::Online,
            in_voice: false,
            deafened: false,
            activities: Vec::new(),
        },
    );

    dispatch_command(
        &restarted,
        "readycheck",
        10,
        "First",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    assert!(
        push_publisher.wait_for_title("⚔️ Readycheck!").await,
        "a hydrated lobby larger than a lowered ready threshold must still push"
    );
}

#[tokio::test]
async fn successful_bell_shortcut_advertises_in_the_persisted_origin_channel() {
    let database = database_with_players(&[(10, "Creator"), (20, "Second Player")]);
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
    dispatch_command(
        &provider,
        "join",
        20,
        "Second Player",
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
    let push_publisher = Arc::new(RecordingPushPublisher::default());
    let push_provider = PushNotificationRegistrationProvider::with_test_publisher(
        database.path(),
        transport.clone(),
        push_publisher.clone(),
    );
    PushNotificationRepository::new(database.path())
        .set_target(
            10,
            Some(42),
            "cama-000000000000000000000000000000000000000000000010",
            1,
        )
        .expect("configure bell push target");
    restarted
        .attach_push_notification_hooks(push_provider.hooks())
        .expect("attach bell push hooks");
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
    assert!(
        push_publisher.wait_for_title("⚔️ Readycheck!").await,
        "the bell shortcut must trigger the same push notification as /readycheck"
    );
}

// The thread "joined." line is always a bare, ping-suppressed @mention now
// (Discord resolves it to the live display name client-side -- see
// slash_join_posts_the_same_thread_line_as_a_sword_react), so these
// exercise the display-name fallback chain through the join observer's
// ConfirmedLobbyJoin.player_display_name instead of the thread message text.
#[tokio::test]
async fn lobby_command_join_uses_interaction_display_name_for_the_confirmed_join_event() {
    let database = database_with_players(&[(10, ".pf")]);
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
        "perry feng",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let confirmed = observer.confirmed.lock().expect("join observer");
    assert_eq!(confirmed.len(), 1);
    assert_eq!(confirmed[0].player_display_name, "perry feng");
}

#[tokio::test]
async fn raw_sword_join_uses_server_nickname_for_the_confirmed_join_event() {
    let database = database_with_players(&[(10, "Creator"), (20, "leafael.")]);
    let transport = Arc::new(RecordingTransport::default());
    transport.set_member(
        42,
        DiscordGuildMemberSnapshot {
            user_id: 20,
            display_name: "Leaf | Atharva".to_owned(),
            presence: DiscordPresence::Online,
            in_voice: false,
            deafened: false,
            activities: Vec::new(),
        },
    );
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
            "leafael.",
        ))
        .await
        .expect("raw join");

    let confirmed = observer.confirmed.lock().expect("join observer");
    assert_eq!(
        confirmed
            .iter()
            .find(|event| event.player_id == 20)
            .map(|event| event.player_display_name.as_str()),
        Some("Leaf | Atharva")
    );
    assert!(
        transport
            .sent_messages()
            .iter()
            .any(|sent| sent.message.response.content == "✅ <@20> joined.")
    );
}

#[tokio::test]
async fn raw_sword_join_falls_back_to_stored_name_when_discord_name_is_unavailable() {
    let database = database_with_players(&[(10, "Creator"), (20, "leafael.")]);
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
    let lobby_message_id = to_u64(
        lobby_snapshot(&provider, LobbyKind::Open)
            .message_ids
            .message_id
            .expect("lobby message")
            .0,
    )
    .expect("Discord lobby message");
    let mut reaction = raw_sword(RawReactionKind::Add, lobby_message_id, 20, "ignored");
    reaction.actor_display_name = None;

    provider
        .raw_reaction_observer()
        .observe(reaction)
        .await
        .expect("raw join");

    let confirmed = observer.confirmed.lock().expect("join observer");
    assert_eq!(
        confirmed
            .iter()
            .find(|event| event.player_id == 20)
            .map(|event| event.player_display_name.as_str()),
        Some("leafael.")
    );
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
    assert!(
        transport
            .sent_messages()
            .iter()
            .any(|sent| sent.message.response.content == "🚪 Recent Join left.")
    );
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
async fn readycheck_uses_discord_display_name_without_server_nickname() {
    let database = database_with_players(&[(10, "Stored Creator")]);
    let transport = Arc::new(RecordingTransport::default());
    transport.set_member_without_nickname(
        42,
        DiscordGuildMemberSnapshot {
            user_id: 10,
            display_name: "Global Creator".to_owned(),
            presence: DiscordPresence::Online,
            in_voice: false,
            deafened: false,
            activities: Vec::new(),
        },
    );
    let provider = provider_for(&database, transport.clone());
    dispatch_command(
        &provider,
        "lobby",
        10,
        "Global Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let lobby = lobby_snapshot(&provider, LobbyKind::Open);
    let (players, _) = provider
        .handler
        .state
        .classify_readycheck_players(&lobby)
        .await;
    assert_eq!(players[&AppUserId(10)].name, "Global Creator");
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
async fn test_curfew_sweep_removes_the_kicked_player_from_the_active_readycheck() {
    // Regression test: a curfew kick must also sweep the removed player out
    // of an in-flight readycheck — its roster and its confirmation reaction —
    // the same way `/kick` does via `sync_readycheck_with_lobby`. Merely
    // resyncing the internal lobby-membership mirror (what the old
    // `refresh_curfew_lobby` did) left a curfewed player still counted
    // toward the ready quorum and still shown as confirmed.
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
    dispatch_command(
        &provider,
        "readycheck",
        99,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    let before = provider
        .handler
        .state
        .readychecks
        .readycheck_generation(scope)
        .expect("readycheck posted");
    assert!(before.lobby_ids.contains(&AppUserId(1)));
    assert!(
        before.reacted.contains_key(&AppUserId(1)),
        "a just-joined player auto-confirms as a recent signup"
    );
    let readycheck_message_id = to_u64(before.message_id.0).expect("readycheck Discord message");

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

    let after = provider
        .handler
        .state
        .readychecks
        .readycheck_generation(scope)
        .expect("readycheck remains live");
    assert!(
        !after.lobby_ids.contains(&AppUserId(1)),
        "curfew kick must remove the player from the readycheck roster"
    );
    assert!(
        !after.reacted.contains_key(&AppUserId(1)),
        "curfew kick must drop the player's readycheck confirmation"
    );
    assert!(
        transport
            .state
            .lock()
            .expect("transport state")
            .removed_reactions
            .iter()
            .any(|(_, message_id, emoji, user_id)| {
                *message_id == readycheck_message_id && emoji.name == READY_EMOJI && *user_id == 1
            }),
        "curfew kick must remove the player's physical readycheck reaction"
    );
    assert!(
        transport.edit_count() > edits_before,
        "the readycheck message must be repainted after a curfew kick"
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

#[tokio::test]
async fn failed_readycheck_publication_releases_the_permit_for_the_next_attempt() {
    // A stale ready check prunes AFK players and then must repaint the lobby
    // display. That repaint is Required, so a Discord failure aborts the run --
    // but the publication permit was already reserved, and leaking it makes
    // every later /readycheck for the scope report "already being published"
    // until a lobby reset or a restart.
    let database = database_with_players(&[(10, "Creator"), (20, "Afk")]);
    let transport = Arc::new(RecordingTransport::default());
    let scope = LobbyScope::new(AppGuildId(42), LobbyKind::Open);
    let long_ago = unix_time_now() - 3_600.0;
    {
        let provider = provider_for(&database, transport.clone());
        create_lobby_and_join_player(&provider, LobbyKind::Open, 10, "Creator", 20, "Afk").await;
    }
    // Age both join times past the recent-join grace window, then hydrate a
    // fresh provider from that persisted state.
    rusqlite::Connection::open(database.path())
        .expect("open lobby database")
        .execute(
            "UPDATE lobby_state SET player_join_times = ?1",
            rusqlite::params![format!("{{\"10\":{long_ago},\"20\":{long_ago}}}")],
        )
        .expect("age persisted join times");
    let provider = provider_for(&database, transport.clone());
    for (user_id, display_name, presence) in [
        (10, "Creator", DiscordPresence::Online),
        (20, "Afk", DiscordPresence::Offline),
    ] {
        transport.set_member(
            42,
            DiscordGuildMemberSnapshot {
                user_id,
                display_name: display_name.to_owned(),
                presence,
                in_voice: false,
                deafened: false,
                activities: Vec::new(),
            },
        );
    }
    // A generation old enough to be stale, which is what enables pruning.
    provider.handler.state.readychecks.set_readycheck_state(
        scope,
        cama_app::readycheck::ReadycheckStateInput {
            message_id: AppMessageId(5_000),
            channel_id: AppChannelId(9),
            lobby_ids: BTreeSet::from([AppUserId(10), AppUserId(20)]),
            player_data: BTreeMap::new(),
            created_at: Some(long_ago),
            initial_reacted: BTreeMap::new(),
        },
    );
    transport.fail_edits();

    let first = dispatch_command_allowing_failure(
        &provider,
        "readycheck",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;
    let second = dispatch_command_allowing_failure(
        &provider,
        "readycheck",
        10,
        "Creator",
        vec![lobby_option(LobbyKind::Open)],
    )
    .await;

    let replies = |responder: &Arc<CapturingResponder>| {
        responder
            .captured
            .lock()
            .expect("responses")
            .followups
            .iter()
            .map(|response| response.content.clone())
            .collect::<Vec<_>>()
    };
    let first_replies = replies(&first);
    assert!(
        first_replies
            .iter()
            .all(|content| !content.starts_with("✅")),
        "the failing lobby repaint must not report success, got {first_replies:?}"
    );
    let in_flight = "⏳ A ready check is already being published. Please try again in a moment.";
    let second_replies = replies(&second);
    assert!(
        !second_replies.iter().any(|content| content == in_flight),
        "the permit must be released after the failed run, got {second_replies:?}"
    );
}
