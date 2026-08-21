use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Semaphore;

use cama_app::dedicated_lobby_channel::{GuildId, LobbyScope, UserId};
use cama_app::draft::{
    DRAFT_POOL_SIZE, DRAFT_TOTAL_PICKS, DraftPhase, DraftStatePersistencePort,
    SqliteDraftStatePersistence,
};
use cama_db::autobet_investments::AutobetInvestmentRepository;
use cama_db::core_repositories::PlayerRepository;
use rusqlite::Connection;

use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessage, DiscordMessageReceipt,
    DiscordMessageSnapshot, DiscordTransport,
};
use crate::lobby_provider::{LobbyRegistrationProvider, LobbyRuntimeConfig};
use crate::registration::{
    InteractionAttachment, InteractionOption, InteractionRequest, InteractionResponseError,
    InteractionValue,
};
use crate::test_support::{FastTestDatabase, fast_database, migrated_database};

struct EmptyMemberSource;

#[async_trait]
impl crate::gateway_events::GuildMemberPageSource for EmptyMemberSource {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<crate::gateway_events::GatewayMember>, String> {
        Ok(Vec::new())
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

#[derive(Default)]
struct TestResponder {
    responses: StdMutex<Vec<InteractionResponse>>,
    updates: StdMutex<Vec<InteractionResponse>>,
    original_edits: StdMutex<Vec<InteractionResponse>>,
    defers: StdMutex<Vec<bool>>,
    fail_defer: bool,
    fail_original_edit: bool,
}

#[async_trait]
impl InteractionResponder for TestResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.responses.lock().expect("responses").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        if self.fail_defer {
            return Err(InteractionResponseError::new("simulated defer failure"));
        }
        self.defers.lock().expect("defers").push(ephemeral);
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

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        if self.fail_original_edit {
            return Err(InteractionResponseError::new(
                "simulated source edit failure",
            ));
        }
        self.original_edits
            .lock()
            .expect("original edits")
            .push(response);
        Ok(())
    }
}

struct NullDiscord {
    sent: Arc<StdMutex<Vec<(u64, DiscordMessage)>>>,
    delivery_receipts: Arc<StdMutex<BTreeMap<String, DiscordMessageReceipt>>>,
    edited: Arc<StdMutex<Vec<(u64, u64, DiscordMessage)>>>,
    deleted: Arc<StdMutex<Vec<(u64, u64)>>>,
    block_completion_sends: AtomicBool,
    completion_sends_started: AtomicUsize,
    completion_send_gate: Semaphore,
    fail_edits: AtomicBool,
    /// Fail edits only for this channel, so one guild's repaint can break
    /// while another guild's succeeds.
    fail_edits_for_channel: AtomicU64,
    fail_delivery_sends: AtomicBool,
    accept_delivery_before_failure: AtomicBool,
    fail_delivery_history: AtomicBool,
    /// A channel Discord itself reports as gone: sends and history probes
    /// fail, and `destination_status` proves the failure is permanent.
    undeliverable_channel: AtomicU64,
}

impl NullDiscord {
    fn is_undeliverable(&self, channel_id: u64) -> bool {
        let undeliverable = self.undeliverable_channel.load(Ordering::Acquire);
        undeliverable != 0 && undeliverable == channel_id
    }
}

impl Default for NullDiscord {
    fn default() -> Self {
        Self {
            sent: Arc::default(),
            delivery_receipts: Arc::default(),
            edited: Arc::default(),
            deleted: Arc::default(),
            block_completion_sends: AtomicBool::new(false),
            completion_sends_started: AtomicUsize::new(0),
            completion_send_gate: Semaphore::new(0),
            fail_edits: AtomicBool::new(false),
            fail_edits_for_channel: AtomicU64::new(0),
            fail_delivery_sends: AtomicBool::new(false),
            accept_delivery_before_failure: AtomicBool::new(false),
            fail_delivery_history: AtomicBool::new(false),
            undeliverable_channel: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl DiscordTransport for NullDiscord {
    async fn fetch_message(
        &self,
        channel_id: u64,
        message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        Ok(Some(DiscordMessageSnapshot {
            receipt: DiscordMessageReceipt {
                channel_id,
                message_id,
                jump_url: format!("https://discord.test/{channel_id}/{message_id}"),
            },
            reactions: Vec::new(),
        }))
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        if self.block_completion_sends.load(Ordering::Acquire) && matches!(channel_id, 700 | 777) {
            self.completion_sends_started.fetch_add(1, Ordering::AcqRel);
            self.completion_send_gate
                .acquire()
                .await
                .map_err(|_| "completion-send test gate closed".to_owned())?
                .forget();
        }
        self.sent.lock().expect("sent").push((channel_id, message));
        Ok(DiscordMessageReceipt {
            channel_id,
            message_id: 1,
            jump_url: format!("https://discord.test/{channel_id}/1"),
        })
    }

    async fn send_message_with_delivery_key(
        &self,
        channel_id: u64,
        delivery_key: &str,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        if self.is_undeliverable(channel_id) {
            return Err("Unknown Channel".to_owned());
        }
        if self.fail_delivery_sends.load(Ordering::Acquire) && channel_id == 700 {
            if self.accept_delivery_before_failure.load(Ordering::Acquire) {
                let receipt = self.send_message(channel_id, message).await?;
                self.delivery_receipts
                    .lock()
                    .expect("delivery receipts")
                    .insert(delivery_key.to_owned(), receipt);
            }
            return Err("simulated ambiguous Discord delivery failure".to_owned());
        }
        if let Some(receipt) = self
            .delivery_receipts
            .lock()
            .expect("delivery receipts")
            .get(delivery_key)
            .cloned()
        {
            return Ok(receipt);
        }
        let receipt = self.send_message(channel_id, message).await?;
        self.delivery_receipts
            .lock()
            .expect("delivery receipts")
            .insert(delivery_key.to_owned(), receipt.clone());
        Ok(receipt)
    }

    async fn find_message_by_delivery_key(
        &self,
        channel_id: u64,
        delivery_key: &str,
        _after_unix_seconds: i64,
        _limit: usize,
    ) -> Result<Option<DiscordMessageReceipt>, String> {
        if self.is_undeliverable(channel_id) {
            return Err("Unknown Channel".to_owned());
        }
        if self.fail_delivery_history.load(Ordering::Acquire) {
            return Err(
                "Discord delivery history search reached its bounded recovery limit".to_owned(),
            );
        }
        Ok(self
            .delivery_receipts
            .lock()
            .expect("delivery receipts")
            .get(delivery_key)
            .filter(|receipt| receipt.channel_id == channel_id)
            .cloned())
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        if self.fail_edits.load(Ordering::Acquire)
            || self.fail_edits_for_channel.load(Ordering::Acquire) == channel_id
        {
            return Err("simulated Discord edit failure".to_owned());
        }
        self.edited
            .lock()
            .expect("edited")
            .push((channel_id, message_id, message));
        Ok(())
    }

    async fn delete_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        self.deleted
            .lock()
            .expect("deleted")
            .push((channel_id, message_id));
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(2)
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

    async fn edit_thread(
        &self,
        _thread_id: u64,
        _name: &str,
        _archived: bool,
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
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        Ok(None)
    }

    async fn destination_status(&self, channel_id: u64) -> DiscordDestinationStatus {
        if self.is_undeliverable(channel_id) {
            DiscordDestinationStatus::Undeliverable
        } else {
            DiscordDestinationStatus::Unknown
        }
    }
}

struct FailingSendDiscord;

#[async_trait]
impl DiscordTransport for FailingSendDiscord {
    async fn fetch_message(
        &self,
        channel_id: u64,
        message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        NullDiscord::default()
            .fetch_message(channel_id, message_id)
            .await
    }

    async fn send_message(
        &self,
        _channel_id: u64,
        _message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        Err("simulated Discord send failure".to_owned())
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        NullDiscord::default()
            .edit_message(channel_id, message_id, message)
            .await
    }

    async fn delete_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        NullDiscord::default()
            .delete_message(channel_id, message_id)
            .await
    }

    async fn create_public_thread(
        &self,
        channel_id: u64,
        message_id: u64,
        name: &str,
    ) -> Result<u64, String> {
        NullDiscord::default()
            .create_public_thread(channel_id, message_id, name)
            .await
    }

    async fn pin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        NullDiscord::default()
            .pin_message(channel_id, message_id)
            .await
    }

    async fn archive_thread(&self, thread_id: u64, name: &str, locked: bool) -> Result<(), String> {
        NullDiscord::default()
            .archive_thread(thread_id, name, locked)
            .await
    }

    async fn add_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        NullDiscord::default()
            .add_reaction(channel_id, message_id, emoji)
            .await
    }

    async fn remove_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
        user_id: u64,
    ) -> Result<(), String> {
        NullDiscord::default()
            .remove_reaction(channel_id, message_id, emoji, user_id)
            .await
    }

    async fn clear_reaction(
        &self,
        channel_id: u64,
        message_id: u64,
        emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        NullDiscord::default()
            .clear_reaction(channel_id, message_id, emoji)
            .await
    }

    async fn unpin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        NullDiscord::default()
            .unpin_message(channel_id, message_id)
            .await
    }

    async fn send_direct_message(
        &self,
        user_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        NullDiscord::default()
            .send_direct_message(user_id, message)
            .await
    }

    async fn guild_member(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        NullDiscord::default().guild_member(guild_id, user_id).await
    }
}

fn provider_fixture() -> (
    FastTestDatabase,
    DraftRegistrationProvider,
    Arc<DraftStateManager>,
    Arc<NullDiscord>,
) {
    let database = fast_database();
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("9001".to_owned()),
        "BOMB_POT_CHANCE" => Some("0".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let drafts = Arc::new(DraftStateManager::default());
    let discord = Arc::new(NullDiscord::default());
    let lobby = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::from([9001]),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 0,
        },
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("lobby provider");
    let provider = DraftRegistrationProvider::new(
        database.path(),
        &config,
        lobby.match_lobby_port(),
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("draft provider");
    (database, provider, drafts, discord)
}

struct RecordingReminderScheduler {
    scheduled: StdMutex<Vec<PendingMatchRecord>>,
}

#[async_trait]
impl DraftReminderScheduler for RecordingReminderScheduler {
    async fn schedule_betting_reminders(&self, pending: PendingMatchRecord) -> Result<(), String> {
        self.scheduled
            .lock()
            .expect("scheduled reminders")
            .push(pending);
        Ok(())
    }
}

struct RecoveryRecordingReminderScheduler {
    normal_calls: AtomicUsize,
    recovery_calls: AtomicUsize,
}

#[async_trait]
impl DraftReminderScheduler for RecoveryRecordingReminderScheduler {
    async fn schedule_betting_reminders(&self, _pending: PendingMatchRecord) -> Result<(), String> {
        self.normal_calls.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    async fn schedule_betting_reminders_recovery(
        &self,
        _pending: PendingMatchRecord,
    ) -> Result<(), String> {
        self.recovery_calls.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[derive(Default)]
struct RecordingNeonObserver {
    events: StdMutex<Vec<String>>,
}

impl RecordingNeonObserver {
    fn result(label: &str) -> DraftNeonResult {
        DraftNeonResult {
            text_block: format!("neon:{label}"),
            attachment: Some(InteractionAttachment::bytes(
                format!("{label}.gif"),
                vec![1, 2, 3],
            )),
        }
    }
}

#[async_trait]
impl DraftNeonObserver for RecordingNeonObserver {
    async fn on_draft_coinflip(
        &self,
        _guild_id: i64,
        _winner_id: i64,
        _loser_id: i64,
    ) -> Result<Option<DraftNeonResult>, String> {
        self.events
            .lock()
            .expect("Neon events")
            .push("coinflip".to_owned());
        Ok(Some(Self::result("coinflip")))
    }

    async fn on_captain_symmetry(
        &self,
        _guild_id: i64,
        _radiant_captain_id: i64,
        _dire_captain_id: i64,
        rating_diff: i64,
    ) -> Result<Option<DraftNeonResult>, String> {
        self.events
            .lock()
            .expect("Neon events")
            .push(format!("symmetry:{rating_diff}"));
        Ok(Some(Self::result("symmetry")))
    }

    async fn on_bomb_pot(
        &self,
        _guild_id: i64,
        pool_amount: i64,
        contributor_count: i64,
    ) -> Result<Option<DraftNeonResult>, String> {
        self.events
            .lock()
            .expect("Neon events")
            .push(format!("bomb:{pool_amount}:{contributor_count}"));
        Ok(Some(Self::result("bomb")))
    }
}

fn provider_fixture_with_scheduler(
    reminders: Arc<dyn DraftReminderScheduler>,
) -> (
    FastTestDatabase,
    DraftRegistrationProvider,
    Arc<DraftStateManager>,
    Arc<NullDiscord>,
) {
    let database = fast_database();
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("9001".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let drafts = Arc::new(DraftStateManager::default());
    let discord = Arc::new(NullDiscord::default());
    let lobby = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::from([9001]),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 0,
        },
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("lobby provider");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler(
        database.path(),
        &config,
        lobby.match_lobby_port(),
        Arc::clone(&drafts),
        discord.clone(),
        reminders,
    )
    .expect("draft provider");
    (database, provider, drafts, discord)
}

fn lobby_request(user_id: u64, name: &str) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: user_id + 10_000,
        name: name.to_owned(),
        user_id,
        user_display_name: format!("User {user_id}"),
        guild_id: Some(42),
        channel_id: Some(777),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "lobby".to_owned(),
            value: InteractionValue::String("open".to_owned()),
        }],
    }
}

async fn populated_lobby_fixture() -> (
    FastTestDatabase,
    LobbyRegistrationProvider,
    MatchLobbyPort,
    Arc<DraftStateManager>,
    Arc<NullDiscord>,
) {
    let database = fast_database();
    seed_players(database.path(), 42, &(1..=10).collect::<Vec<_>>(), 100);
    let drafts = Arc::new(DraftStateManager::default());
    let discord = Arc::new(NullDiscord::default());
    let lobby = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::from([9001]),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 0,
        },
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("lobby provider");
    let mut builder = RegistryBuilder::default();
    lobby.register(&mut builder).expect("register lobby");
    let registry = builder.build();
    let handler = registry.command_handler("lobby").expect("lobby handler");
    handler
        .handle(
            lobby_request(1, "lobby"),
            Arc::new(TestResponder::default()),
        )
        .await
        .expect("create lobby");
    let service = lobby.live_lobby_service();
    let scope = LobbyScope::new(GuildId(42), AppLobbyKind::Open);
    for user_id in 2..=10 {
        assert!(service.join_lobby(UserId(user_id), scope).success);
    }
    let port = lobby.match_lobby_port();
    assert_eq!(
        port.snapshot(42, AppLobbyKind::Open)
            .expect("lobby")
            .player_ids
            .len(),
        10
    );
    (database, lobby, port, drafts, discord)
}

fn draft_request(
    user_id: u64,
    guild_id: Option<u64>,
    permissions: Option<u64>,
    subcommand: &str,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: user_id + 100,
        name: "draft".to_owned(),
        user_id,
        user_display_name: format!("User {user_id}"),
        guild_id,
        channel_id: Some(777),
        member_permissions: permissions,
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(Vec::new()),
        }],
    }
}

fn state_for_draft() -> DraftState {
    let mut state = DraftState::with_session(42, DraftLobbyKind::Open, 9);
    state.player_pool_ids = (1..=10).collect();
    state.captain1_id = Some(1);
    state.captain2_id = Some(2);
    state.radiant_captain_id = Some(1);
    state.dire_captain_id = Some(2);
    state.coinflip_winner_id = Some(1);
    state.captain1_rating = 1600.0;
    state.captain2_rating = 1500.0;
    for id in 1..=10 {
        state.player_pool_data.insert(
            id,
            PlayerPoolEntry {
                name: format!("P{id}"),
                rating: 1500.0 + id as f64,
                roles: vec!["1".to_owned()],
            },
        );
    }
    state
}

async fn persistent_completion_fixture() -> (
    FastTestDatabase,
    DraftRegistrationProvider,
    Arc<DraftStateManager>,
    Arc<SqliteDraftStatePersistence>,
    Arc<StdMutex<DraftState>>,
    DraftState,
    Arc<NullDiscord>,
) {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BOMB_POT_CHANCE" => Some("0".to_owned()),
        "AUTO_SPECTATOR_BET_ENABLED" => Some("false".to_owned()),
        "FIRST_GAME_POOL_DAILY_AMOUNT" => Some("0".to_owned()),
        _ => None,
    })
    .expect("persistent completion config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.phase = DraftPhase::Complete;
    state.current_pick_index = DRAFT_TOTAL_PICKS;
    state.radiant_player_ids = vec![1, 3, 5, 7, 9];
    state.dire_player_ids = vec![2, 4, 6, 8, 10];
    state.radiant_hero_pick_order = Some(1);
    state.dire_hero_pick_order = Some(2);
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(55);
    let persisted = persistence
        .create_envelope(&state.to_snapshot().envelope(1))
        .expect("create durable completion draft");
    let state = persisted.state.clone().into_state();
    let handle = drafts
        .restore_draft(state.clone())
        .expect("restore durable completion draft");
    assert!(lobbies.reserve_players(
        42,
        AppLobbyKind::Open,
        &state.player_pool_ids.iter().copied().collect()
    ));
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies,
        Arc::clone(&drafts),
        discord.clone(),
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("persistent completion provider");
    lock_recover(&provider.handler.persisted_envelopes).insert((42, state.session_id), persisted);
    lock_recover(&provider.handler.draft_messages).insert(
        (42, state.session_id),
        DiscordMessageReceipt {
            channel_id: 777,
            message_id: 55,
            jump_url: "https://discord.test/channels/42/777/55".to_owned(),
        },
    );
    (
        database,
        provider,
        drafts,
        persistence,
        handle,
        state,
        discord,
    )
}

#[test]
fn provider_recreation_hydrates_state_phase_message_and_deadline() {
    let database = migrated_database();
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("9001".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    // Hydration must re-reserve a live lobby.  This fixture deliberately has
    // no player pool so it isolates restart state/message/deadline recovery.
    state.player_pool_ids.clear();
    state.phase = DraftPhase::WinnerSideChoice;
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    let deadline = unix_now().saturating_add(120);
    let mut requested = state.to_snapshot().envelope(1);
    requested.deadline_at = Some(deadline);
    let persisted = persistence
        .create_envelope(&requested)
        .expect("create durable draft");

    let drafts = Arc::new(DraftStateManager::default());
    let discord = Arc::new(NullDiscord::default());
    let lobby = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::from([9001]),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 0,
        },
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("lobby provider");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobby.match_lobby_port(),
        Arc::clone(&drafts),
        discord.clone(),
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");

    assert_eq!(block_on(provider.hydrate()).expect("hydrate draft"), 1);
    let restored = drafts.get_state(Some(42)).expect("restored state");
    let restored = restored.lock().expect("restored state lock").clone();
    assert_eq!(restored.session_id, persisted.session_id);
    assert_eq!(restored.phase, DraftPhase::WinnerSideChoice);
    assert_eq!(restored.draft_channel_id, Some(777));
    assert_eq!(restored.draft_message_id, Some(1234));
    let loaded = persistence.load_all().expect("load durable draft");
    assert_eq!(loaded[0].deadline_at, Some(deadline));
    assert!(
        discord
            .edited
            .lock()
            .expect("edited messages")
            .iter()
            .any(|(_, message_id, _)| *message_id == 1234)
    );

    // Repeated READY/hydration is idempotent for the same durable session.
    assert_eq!(block_on(provider.hydrate()).expect("rehydrate draft"), 1);
    assert_eq!(
        drafts
            .get_state(Some(42))
            .expect("rehydrated state")
            .lock()
            .expect("rehydrated state lock")
            .session_id,
        persisted.session_id
    );

    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register draft");
    let handler = builder
        .build()
        .component_handler("draft")
        .expect("component handler");
    let legacy = Arc::new(TestResponder::default());
    block_on(handler.handle(
        InteractionRequest::Component {
            interaction_id: 91,
            custom_id: "draft_choice_side".to_owned(),
            user_id: 1,
            user_display_name: "Captain".to_owned(),
            guild_id: Some(42),
            channel_id: Some(777),
            member_permissions: None,
            values: Vec::new(),
        },
        legacy.clone(),
    ))
    .expect("legacy callback response");
    assert!(
        legacy.responses.lock().expect("legacy response")[0]
            .content
            .contains("current draft")
    );

    // Two callbacks for the same durable session serialize the local
    // transition and CAS. The second callback observes the first callback's
    // committed phase and cannot clobber it.
    let first = Arc::new(TestResponder::default());
    let second = Arc::new(TestResponder::default());
    let first_request = InteractionRequest::Component {
        interaction_id: 92,
        custom_id: format!("draft:{}:side:radiant", persisted.session_id),
        user_id: 1,
        user_display_name: "Captain".to_owned(),
        guild_id: Some(42),
        channel_id: Some(777),
        member_permissions: None,
        values: Vec::new(),
    };
    let second_request = InteractionRequest::Component {
        interaction_id: 93,
        custom_id: format!("draft:{}:side:dire", persisted.session_id),
        user_id: 1,
        user_display_name: "Captain".to_owned(),
        guild_id: Some(42),
        channel_id: Some(777),
        member_permissions: None,
        values: Vec::new(),
    };
    let (first_result, second_result) = block_on(async {
        tokio::join!(
            handler.handle(first_request, first.clone()),
            handler.handle(second_request, second.clone()),
        )
    });
    first_result.expect("first concurrent callback");
    second_result.expect("second concurrent callback");
    let final_state = drafts.get_state(Some(42)).expect("final concurrent state");
    assert_eq!(
        final_state.lock().expect("final state lock").phase,
        DraftPhase::LoserChoice
    );
    assert_eq!(
        persistence.load_all().expect("load CAS state")[0].revision,
        2
    );

    discord.fail_edits.store(true, Ordering::Release);
    let repaint_error = block_on(provider.hydrate()).expect_err("repaint must fail recovery");
    assert!(repaint_error.contains("simulated Discord edit failure"));
    assert!(drafts.has_active_draft(Some(42)));
    assert_eq!(
        persistence
            .load_all()
            .expect("durable state retained")
            .len(),
        1
    );
    assert!(
        provider
            .handler
            .persisted_envelopes
            .lock()
            .expect("persisted envelopes")
            .contains_key(&(42, persisted.session_id))
    );
    assert!(
        provider
            .handler
            .draft_messages
            .lock()
            .expect("draft messages")
            .contains_key(&(42, persisted.session_id))
    );
    assert!(
        provider
            .handler
            .draft_operation_locks
            .lock()
            .expect("draft operation locks")
            .contains_key(&(42, persisted.session_id))
    );
    assert!(
        provider
            .handler
            .timeout_tasks
            .lock()
            .expect("timeout tasks")
            .contains_key(&42)
    );
    discord.fail_edits.store(false, Ordering::Release);

    {
        let handle = drafts.get_state(Some(42)).expect("live state");
        let mut live = handle.lock().expect("live state lock");
        live.draft_channel_id = None;
        live.draft_message_id = None;
    }
    let live_mismatch_error = block_on(provider.hydrate())
        .expect_err("repeated hydration must reject a changed live state");
    assert!(live_mismatch_error.contains("live state does not match its CAS cache"));
    assert!(
        provider
            .handler
            .draft_messages
            .lock()
            .expect("draft messages")
            .contains_key(&(42, persisted.session_id))
    );
}

#[test]
fn draft_ready_observer_hydrates_active_rows_idempotently() {
    let database = migrated_database();
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("9001".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.player_pool_ids.clear();
    state.phase = DraftPhase::WinnerSideChoice;
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    persistence
        .create_envelope(&state.to_snapshot().envelope(1))
        .expect("create durable draft");
    let drafts = Arc::new(DraftStateManager::default());
    let discord = Arc::new(NullDiscord::default());
    let lobby = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::from([9001]),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 0,
        },
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("lobby provider");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobby.match_lobby_port(),
        Arc::clone(&drafts),
        discord,
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence),
    )
    .expect("draft provider");
    let observer = provider.gateway_observer();
    let context =
        || crate::gateway_events::ReadyRecoveryContext::new(vec![42], Arc::new(EmptyMemberSource));
    let first = block_on(observer.ready_recovery(context()));
    assert_eq!(first.guilds_refreshed, 1);
    assert!(first.failures.is_empty());
    let session_id = drafts
        .get_state(Some(42))
        .expect("hydrated state")
        .lock()
        .expect("state lock")
        .session_id;
    let second = block_on(observer.ready_recovery(context()));
    assert_eq!(second.guilds_refreshed, 1);
    assert!(second.failures.is_empty());
    assert_eq!(
        drafts
            .get_state(Some(42))
            .expect("repeated hydrated state")
            .lock()
            .expect("state lock")
            .session_id,
        session_id
    );
}

#[test]
fn draft_ready_observer_ignores_foreign_guild_rows_before_transport() {
    let database = migrated_database();
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("9001".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = DraftState::with_session(43, DraftLobbyKind::Open, 10);
    state.phase = DraftPhase::WinnerSideChoice;
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    persistence
        .create_envelope(&state.to_snapshot().envelope(1))
        .expect("create foreign durable draft");
    let mut finalizing = DraftState::with_session(44, DraftLobbyKind::Open, 11);
    finalizing.phase = DraftPhase::Complete;
    let mut finalizing_envelope = finalizing.to_snapshot().envelope(1);
    finalizing_envelope.finalizing = true;
    persistence
        .create_envelope(&finalizing_envelope)
        .expect("create foreign finalizing draft");
    let drafts = Arc::new(DraftStateManager::default());
    let discord = Arc::new(NullDiscord::default());
    let lobby = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::from([9001]),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 0,
        },
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("lobby provider");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobby.match_lobby_port(),
        Arc::clone(&drafts),
        discord.clone(),
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");

    let report = block_on(provider.gateway_observer().ready_recovery(
        crate::gateway_events::ReadyRecoveryContext::new(vec![42], Arc::new(EmptyMemberSource)),
    ));

    assert!(report.failures.is_empty());
    assert_eq!(report.guilds_attempted, 1);
    assert_eq!(report.guilds_refreshed, 1);
    assert!(drafts.get_state(Some(43)).is_none());
    assert!(drafts.get_state(Some(44)).is_none());
    assert_eq!(
        persistence.load_all().expect("foreign rows retained").len(),
        2
    );
    assert!(discord.sent.lock().expect("sent messages").is_empty());
    assert!(discord.edited.lock().expect("edited messages").is_empty());
}

#[test]
fn production_main_composes_draft_sqlite_recovery_and_ready_observer() {
    let source = include_str!("../main.rs");
    assert!(source.contains("SqliteDraftStatePersistence::new(&config.db_path)"));
    assert!(source.contains("with_persistence(Arc::new(SqliteDraftStatePersistence"));
    assert!(source.contains("draft_provider.gateway_observer()"));
}

#[tokio::test]
async fn hydration_reserves_nonempty_pool_and_repeated_ready_keeps_live_reservation() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("9001".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.phase = DraftPhase::WinnerSideChoice;
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    let mut requested = state.to_snapshot().envelope(1);
    requested.deadline_at = Some(unix_now().saturating_add(120));
    let persisted = persistence
        .create_envelope(&requested)
        .expect("create durable draft");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies.clone(),
        Arc::clone(&drafts),
        discord.clone(),
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");

    assert_eq!(provider.hydrate().await.expect("first hydration"), 1);
    let player_ids = (1..=10).collect::<BTreeSet<_>>();
    assert!(!lobbies.reserve_players(42, AppLobbyKind::Open, &player_ids));
    assert_eq!(provider.hydrate().await.expect("repeated hydration"), 1);
    assert!(!lobbies.reserve_players(42, AppLobbyKind::Open, &player_ids));
    assert_eq!(
        drafts
            .get_state(Some(42))
            .expect("live hydrated draft")
            .lock()
            .expect("live hydrated draft lock")
            .session_id,
        persisted.session_id
    );
    discord.fail_edits.store(true, Ordering::Release);
    let error = provider
        .hydrate()
        .await
        .expect_err("repeated repaint failure must be reported");
    assert!(error.contains("simulated Discord edit failure"));
    assert!(drafts.has_active_draft(Some(42)));
    assert!(!lobbies.reserve_players(42, AppLobbyKind::Open, &player_ids));
}

#[tokio::test]
async fn initial_hydration_repaint_failure_unwinds_runtime_resources_but_retains_row() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.phase = DraftPhase::WinnerSideChoice;
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    let persisted = persistence
        .create_envelope(&state.to_snapshot().envelope(1))
        .expect("create durable draft");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies.clone(),
        Arc::clone(&drafts),
        discord.clone(),
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");

    discord.fail_edits.store(true, Ordering::Release);
    let error = provider
        .hydrate()
        .await
        .expect_err("initial repaint must fail recovery");
    assert!(error.contains("simulated Discord edit failure"));
    assert!(!drafts.has_active_draft(Some(42)));
    assert_eq!(
        persistence.load_all().expect("durable row retained").len(),
        1
    );
    assert!(
        !provider
            .handler
            .persisted_envelopes
            .lock()
            .expect("persisted envelopes")
            .contains_key(&(42, persisted.session_id))
    );
    assert!(
        !provider
            .handler
            .draft_messages
            .lock()
            .expect("draft messages")
            .contains_key(&(42, persisted.session_id))
    );
    assert!(
        !provider
            .handler
            .timeout_tasks
            .lock()
            .expect("timeout tasks")
            .contains_key(&42)
    );
    assert!(
        !provider
            .handler
            .draft_operation_locks
            .lock()
            .expect("draft operation locks")
            .contains_key(&(42, persisted.session_id))
    );
    let player_ids = (1..=10).collect::<BTreeSet<_>>();
    assert!(lobbies.reserve_players(42, AppLobbyKind::Open, &player_ids));
    lobbies.release_players(42, AppLobbyKind::Open, &player_ids);

    discord.fail_edits.store(false, Ordering::Release);
    assert_eq!(provider.hydrate().await.expect("retry hydration"), 1);
}

#[tokio::test]
async fn initial_hydration_missing_source_unwinds_runtime_resources_but_retains_row() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.phase = DraftPhase::WinnerSideChoice;
    let persisted = persistence
        .create_envelope(&state.to_snapshot().envelope(1))
        .expect("create durable draft without source identity");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies.clone(),
        Arc::clone(&drafts),
        discord,
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");

    let error = provider
        .hydrate()
        .await
        .expect_err("missing source must fail recovery");
    assert!(error.contains("no source message identity"));
    assert!(!drafts.has_active_draft(Some(42)));
    assert_eq!(
        persistence.load_all().expect("durable row retained").len(),
        1
    );
    assert!(
        !provider
            .handler
            .persisted_envelopes
            .lock()
            .expect("persisted envelopes")
            .contains_key(&(42, persisted.session_id))
    );
    assert!(
        !provider
            .handler
            .draft_messages
            .lock()
            .expect("draft messages")
            .contains_key(&(42, persisted.session_id))
    );
    assert!(
        !provider
            .handler
            .timeout_tasks
            .lock()
            .expect("timeout tasks")
            .contains_key(&42)
    );
    assert!(
        !provider
            .handler
            .draft_operation_locks
            .lock()
            .expect("draft operation locks")
            .contains_key(&(42, persisted.session_id))
    );
    let player_ids = (1..=10).collect::<BTreeSet<_>>();
    assert!(lobbies.reserve_players(42, AppLobbyKind::Open, &player_ids));
    lobbies.release_players(42, AppLobbyKind::Open, &player_ids);
}

#[tokio::test]
async fn hydration_stale_loaded_session_cleans_its_unused_operation_lock() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut durable_state = state_for_draft();
    durable_state.player_pool_ids.clear();
    durable_state.phase = DraftPhase::WinnerSideChoice;
    durable_state.draft_channel_id = Some(777);
    durable_state.draft_message_id = Some(1234);
    let persisted = persistence
        .create_envelope(&durable_state.to_snapshot().envelope(1))
        .expect("create durable draft");
    let competing = DraftState::with_session(
        42,
        DraftLobbyKind::Open,
        persisted.session_id.saturating_add(1),
    );
    drafts
        .restore_draft(competing)
        .expect("install competing live session");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies,
        Arc::clone(&drafts),
        discord,
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");

    let error = provider
        .hydrate()
        .await
        .expect_err("stale durable session must fail closed");
    assert!(error.contains("refusing to hydrate stale session"));
    assert!(
        !provider
            .handler
            .draft_operation_locks
            .lock()
            .expect("draft operation locks")
            .contains_key(&(42, persisted.session_id))
    );
    assert_eq!(
        persistence.load_all().expect("durable row retained").len(),
        1
    );
}

#[tokio::test]
async fn component_cas_loser_rolls_back_local_state_without_discord_publication() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.phase = DraftPhase::WinnerSideChoice;
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    let persisted = persistence
        .create_envelope(&state.to_snapshot().envelope(1))
        .expect("create durable draft");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies,
        Arc::clone(&drafts),
        discord,
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");
    provider.hydrate().await.expect("hydrate draft");
    let before = drafts
        .get_state(Some(42))
        .expect("live draft")
        .lock()
        .expect("live draft lock")
        .clone();

    let mut external = persisted.clone();
    external.application_metadata.insert(
        "cas_winner".to_owned(),
        serde_json::Value::String("external writer".to_owned()),
    );
    let durable_winner = persistence
        .replace_envelope_if_revision(
            persisted.guild_id,
            persisted.session_id,
            persisted.revision,
            &external,
        )
        .expect("external CAS winner");
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register draft");
    let handler = builder
        .build()
        .component_handler("draft")
        .expect("component handler");
    let responder = Arc::new(TestResponder::default());
    let error = handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 95,
                custom_id: format!("draft:{}:side:radiant", persisted.session_id),
                user_id: 1,
                user_display_name: "Captain".to_owned(),
                guild_id: Some(42),
                channel_id: Some(777),
                member_permissions: None,
                values: Vec::new(),
            },
            responder.clone(),
        )
        .await
        .expect_err("stale provider CAS must lose");
    assert!(error.to_string().contains("revision"));
    assert_eq!(
        drafts
            .get_state(Some(42))
            .expect("live draft after CAS loss")
            .lock()
            .expect("live draft lock after CAS loss")
            .clone(),
        before
    );
    assert_eq!(
        persistence.load_all().expect("durable CAS winner")[0],
        durable_winner
    );
    assert!(responder.updates.lock().expect("updates").is_empty());
    let mismatch = provider
        .hydrate()
        .await
        .expect_err("repeated hydration must reject stale cache revision");
    assert!(mismatch.contains("durable envelope does not match"));
}

#[tokio::test]
async fn final_pick_persists_recovery_fence_before_completion_side_effects() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BOMB_POT_CHANCE" => Some("0".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.phase = DraftPhase::Drafting;
    state.current_pick_index = 7;
    state.current_round_first_captain_id = Some(1);
    state.radiant_player_ids = vec![1, 3, 5, 7, 9];
    state.dire_player_ids = vec![2, 4, 6, 8];
    state.radiant_hero_pick_order = Some(1);
    state.dire_hero_pick_order = Some(2);
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    let mut requested = state.to_snapshot().envelope(1);
    requested.deadline_at = Some(unix_now().saturating_add(120));
    let persisted = persistence
        .create_envelope(&requested)
        .expect("create durable final-pick draft");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies.clone(),
        Arc::clone(&drafts),
        discord,
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");
    provider.hydrate().await.expect("hydrate final-pick draft");
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register draft");
    let handler = builder
        .build()
        .component_handler("draft")
        .expect("component handler");
    let lobby_lock = lobbies.operation_lock(42, AppLobbyKind::Open);
    let lobby_guard = lobby_lock.lock().await;
    let responder = Arc::new(TestResponder::default());
    let task = tokio::spawn({
        let responder = responder.clone();
        async move {
            handler
                .handle(
                    InteractionRequest::Component {
                        interaction_id: 94,
                        custom_id: format!("draft:{}:pick:10", persisted.session_id),
                        user_id: 2,
                        user_display_name: "Captain".to_owned(),
                        guild_id: Some(42),
                        channel_id: Some(777),
                        member_permissions: None,
                        values: Vec::new(),
                    },
                    responder,
                )
                .await
        }
    });

    let mut fenced = None;
    for _ in 0..100 {
        let rows = persistence.load_all().expect("load final-pick fence");
        if let Some(envelope) = rows.into_iter().next()
            && envelope.finalizing
            && envelope.state.as_state().phase == DraftPhase::Complete
        {
            fenced = Some(envelope);
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let fenced = fenced.expect("final pick recovery fence");
    assert_eq!(fenced.pending_match_id, None);
    let observer = provider.gateway_observer();
    let ready_task = tokio::spawn(async move {
        observer
            .ready_recovery(crate::gateway_events::ReadyRecoveryContext::new(
                vec![42],
                Arc::new(EmptyMemberSource),
            ))
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !ready_task.is_finished(),
        "READY must wait behind the in-flight final-pick operation fence"
    );
    drop(lobby_guard);
    task.await
        .expect("final-pick callback task")
        .expect("final-pick callback");
    let ready = ready_task.await.expect("READY recovery task");
    assert!(
        ready.failures.is_empty(),
        "READY recovery failed: {:?}",
        ready.failures
    );
}

#[tokio::test]
async fn provider_linked_retry_reuses_frozen_plan_despite_opposite_clock_rng_and_destinations() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BOMB_POT_CHANCE" => Some("0".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.phase = DraftPhase::Complete;
    state.current_pick_index = DRAFT_TOTAL_PICKS;
    state.radiant_player_ids = vec![1, 3, 5, 7, 9];
    state.dire_player_ids = vec![2, 4, 6, 8, 10];
    state.radiant_hero_pick_order = Some(1);
    state.dire_hero_pick_order = Some(2);
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    let mut requested = state.to_snapshot().envelope(1);
    requested.finalizing = true;
    requested.deadline_at = None;
    let persisted = persistence
        .create_envelope(&requested)
        .expect("create fenced draft");
    let state = persisted.state.clone().into_state();
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies,
        drafts,
        discord,
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");
    provider
        .handler
        .persisted_envelopes
        .lock()
        .expect("persisted envelopes")
        .insert((42, state.session_id), persisted);
    let pending_state = db_pending_state(
        &state,
        1_700_000_000,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state");

    let first = provider
        .handler
        .link_finalizing_pending_match(
            &state,
            Some(&pending_state),
            Some(700),
            Some(701),
            Some(702),
        )
        .await
        .expect("first atomic pending link");
    provider
        .handler
        .run_persisted_financial_setup(&state, first.pending.pending_match_id)
        .await
        .expect("post-link financial setup");
    let connection = Connection::open(database.path()).expect("open linked pending database");
    let raw_payload = connection
        .query_row(
            "SELECT payload FROM pending_matches WHERE pending_match_id=?1",
            [first.pending.pending_match_id],
            |row| row.get::<_, String>(0),
        )
        .expect("load linked pending payload");
    let raw_plan = connection
        .query_row(
            "SELECT plan_json FROM draft_finalization_jobs WHERE pending_match_id=?1",
            [first.pending.pending_match_id],
            |row| row.get::<_, String>(0),
        )
        .expect("load linked finalization plan");
    let typed_plan = DraftFinalizationPlan::from_json(&raw_plan).expect("decode typed plan");
    assert_eq!(typed_plan.started_at, 1_700_000_000);
    assert_eq!(typed_plan.shuffle_timestamp, 1_700_000_000);
    assert_eq!(
        typed_plan.bet_lock_until,
        1_700_000_000 + provider.handler.config.bet_lock_seconds
    );
    assert!(!typed_plan.is_bomb_pot);
    assert_eq!(typed_plan.lobby.channel_id, Some(700));
    assert_eq!(typed_plan.origin.channel_id, Some(701));
    assert_eq!(typed_plan.thread_embed.channel_id, Some(702));
    assert_eq!(typed_plan.source.channel_id, state.draft_channel_id);
    assert_eq!(typed_plan.source.message_id, state.draft_message_id);
    drop(connection);
    let retry_config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BOMB_POT_CHANCE" => Some("1".to_owned()),
        "AUTO_SPECTATOR_BET_ENABLED" => Some("true".to_owned()),
        "FIRST_GAME_POOL_DAILY_AMOUNT" => Some("999999".to_owned()),
        _ => None,
    })
    .expect("opposite retry config");
    let retry_provider =
        DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
            database.path(),
            &retry_config,
            provider.handler.lobbies.clone(),
            Arc::clone(&provider.handler.drafts),
            Arc::clone(&provider.handler.discord),
            Arc::new(NoopDraftReminderScheduler),
            Arc::new(NoopDraftNeonObserver),
            Some(persistence),
        )
        .expect("opposite retry provider");
    let linked_envelope = lock_recover(&provider.handler.persisted_envelopes)
        .get(&(42, state.session_id))
        .cloned()
        .expect("first-writer linked envelope");
    lock_recover(&retry_provider.handler.persisted_envelopes)
        .insert((42, state.session_id), linked_envelope);
    let retried = retry_provider
        .handler
        .link_finalizing_pending_match(&state, None, Some(900), Some(901), Some(902))
        .await
        .expect("retry atomic pending link");
    assert_eq!(
        retried.pending.pending_match_id,
        first.pending.pending_match_id
    );
    assert_eq!(
        retry_provider
            .handler
            .pending
            .pending_matches(42)
            .expect("pending matches")
            .len(),
        1
    );
    let cached = retry_provider
        .handler
        .persisted_envelopes
        .lock()
        .expect("persisted envelopes")
        .get(&(42, state.session_id))
        .cloned()
        .expect("linked envelope");
    assert_eq!(
        cached.pending_match_id,
        Some(first.pending.pending_match_id)
    );
    assert_eq!(cached.revision, 2);
    assert_eq!(retried.plan, typed_plan);
    assert_eq!(retried.plan.started_at, 1_700_000_000);
    assert_eq!(retried.plan.shuffle_timestamp, 1_700_000_000);
    assert!(!retried.plan.is_bomb_pot);
    assert_eq!(retried.plan.lobby.channel_id, Some(700));
    assert_eq!(retried.plan.origin.channel_id, Some(701));
    assert_eq!(retried.plan.thread_embed.channel_id, Some(702));
    let completion_key = Connection::open(database.path())
        .expect("open database")
        .query_row(
            "SELECT completion_key FROM pending_matches WHERE pending_match_id=?1",
            [first.pending.pending_match_id],
            |row| row.get::<_, String>(0),
        )
        .expect("completion key");
    assert_eq!(completion_key, format!("draft:42:{}", state.session_id));
    let preserved_raw = Connection::open(database.path())
        .expect("reopen database")
        .query_row(
            "SELECT payload FROM pending_matches WHERE pending_match_id=?1",
            [first.pending.pending_match_id],
            |row| row.get::<_, String>(0),
        )
        .expect("reload preserved pending payload");
    assert_eq!(preserved_raw, raw_payload);
    let preserved_plan = Connection::open(database.path())
        .expect("reopen database for plan")
        .query_row(
            "SELECT plan_json FROM draft_finalization_jobs WHERE pending_match_id=?1",
            [first.pending.pending_match_id],
            |row| row.get::<_, String>(0),
        )
        .expect("reload preserved finalization plan");
    assert_eq!(preserved_plan, raw_plan);
}

#[test]
fn financial_setup_policy_uses_frozen_shuffle_timestamp_at_fixed_pst_boundary() {
    let (_database, provider, _drafts, _discord) =
        provider_fixture_with_scheduler(Arc::new(NoopDraftReminderScheduler));
    let state = state_for_draft();
    let before_rollover = db_pending_state(
        &state,
        1_786_708_799,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state before fixed-PST rollover");
    let at_rollover = db_pending_state(
        &state,
        1_786_708_800,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state at fixed-PST rollover");

    assert_eq!(
        provider
            .handler
            .financial_setup_policy(&before_rollover)
            .expect("policy before rollover")
            .game_date,
        "2026-08-13"
    );
    assert_eq!(
        provider
            .handler
            .financial_setup_policy(&at_rollover)
            .expect("policy at rollover")
            .game_date,
        "2026-08-14"
    );
}

#[tokio::test]
async fn persistent_completion_atomically_marks_terminal_and_deletes_envelope() {
    let (database, provider, drafts, persistence, handle, state, _discord) =
        persistent_completion_fixture().await;
    provider
        .handler
        .complete_owned(
            42,
            handle,
            state.clone(),
            Arc::new(TestResponder::default()),
            true,
        )
        .await
        .expect("complete persisted draft");

    let completion_key = cama_db::draft_finalization::draft_completion_key(42, state.session_id);
    let finalization =
        cama_db::draft_finalization::DraftFinalizationRepository::new(database.path());
    let job = finalization
        .job(&completion_key)
        .expect("load terminal job")
        .expect("terminal job");
    assert_eq!(
        job.stage,
        cama_db::draft_finalization::DRAFT_FINALIZATION_COMPLETE_STAGE
    );
    let pending = provider
        .handler
        .pending
        .pending_match(42, job.pending_match_id)
        .expect("load published pending match")
        .expect("published pending match");
    assert_eq!(pending.state.thread_shuffle_thread_id, Some(2));
    assert_eq!(pending.state.thread_shuffle_message_id, Some(1));
    assert!(
        finalization
            .load_incomplete_jobs()
            .expect("load incomplete jobs")
            .is_empty()
    );
    assert!(
        persistence
            .load_all()
            .expect("load Draft envelopes")
            .is_empty()
    );
    assert!(!drafts.has_active_draft(Some(42)));
}

#[tokio::test]
async fn terminal_job_cas_failure_retains_linked_job_and_envelope() {
    let (database, provider, _drafts, persistence, handle, state, _discord) =
        persistent_completion_fixture().await;
    Connection::open(database.path())
        .expect("open terminal-failure fixture")
        .execute_batch(
            "CREATE TRIGGER fail_draft_job_completion
             BEFORE UPDATE OF stage ON draft_finalization_jobs
             WHEN NEW.stage = 'complete'
             BEGIN
                 SELECT RAISE(ABORT, 'simulated terminal job CAS failure');
             END;",
        )
        .expect("install terminal-failure trigger");

    let error = provider
        .handler
        .complete_owned(
            42,
            handle,
            state.clone(),
            Arc::new(TestResponder::default()),
            true,
        )
        .await
        .expect_err("terminal job CAS must fail");
    assert!(error.contains("simulated terminal job CAS failure"));

    let completion_key = cama_db::draft_finalization::draft_completion_key(42, state.session_id);
    let finalization =
        cama_db::draft_finalization::DraftFinalizationRepository::new(database.path());
    let job = finalization
        .job(&completion_key)
        .expect("load retained job")
        .expect("retained job");
    assert_eq!(
        job.stage,
        cama_db::draft_finalization::DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE
    );
    assert_eq!(
        finalization
            .load_incomplete_jobs()
            .expect("load retained incomplete jobs"),
        vec![job.clone()]
    );
    let envelopes = persistence.load_all().expect("load retained envelope");
    assert_eq!(envelopes.len(), 1);
    assert!(envelopes[0].finalizing);
    assert_eq!(envelopes[0].pending_match_id, Some(job.pending_match_id));
}

#[tokio::test]
async fn financial_setup_failure_retains_live_draft_then_retry_completes_once() {
    let (database, provider, drafts, persistence, handle, state, _discord) =
        persistent_completion_fixture().await;
    Connection::open(database.path())
        .expect("open setup-failure fixture")
        .execute_batch(
            "CREATE TRIGGER fail_draft_financial_setup
             BEFORE INSERT ON draft_financial_effects
             BEGIN
                 SELECT RAISE(ABORT, 'simulated Draft financial setup failure');
             END;",
        )
        .expect("install setup-failure trigger");

    let responder = Arc::new(TestResponder::default());
    provider
        .handler
        .complete_owned(
            42,
            Arc::clone(&handle),
            state.clone(),
            responder.clone(),
            true,
        )
        .await
        .expect("setup failure returns retry guidance");

    assert!(drafts.has_active_draft(Some(42)));
    let retained_lobby = provider
        .handler
        .lobbies
        .snapshot(42, AppLobbyKind::Open)
        .expect("retained lobby");
    assert_eq!(retained_lobby.player_ids.len(), 10);
    let envelopes = persistence
        .load_all()
        .expect("load retained Draft envelope");
    assert_eq!(envelopes.len(), 1);
    assert!(envelopes[0].finalizing);
    let pending_match_id = envelopes[0]
        .pending_match_id
        .expect("retained linked pending match");
    let completion_key = cama_db::draft_finalization::draft_completion_key(42, state.session_id);
    let finalization =
        cama_db::draft_finalization::DraftFinalizationRepository::new(database.path());
    let linked_job = finalization
        .job(&completion_key)
        .expect("load linked job")
        .expect("linked job");
    assert_eq!(
        linked_job.stage,
        cama_db::draft_finalization::DRAFT_FINALIZATION_INITIAL_STAGE
    );
    let connection = Connection::open(database.path()).expect("inspect rolled-back setup");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM draft_financial_effects", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count rolled-back effects"),
        0
    );
    assert_eq!(
        provider
            .handler
            .pending
            .pending_matches(42)
            .expect("retained pending matches")
            .len(),
        1
    );
    {
        let response = responder.responses.lock().expect("retry responses");
        assert_eq!(response.len(), 1);
        assert!(response[0].content.contains("safely retained"));
        assert!(response[0].content.contains("Retry finalization"));
    }
    connection
        .execute_batch("DROP TRIGGER fail_draft_financial_setup;")
        .expect("remove setup-failure trigger");
    drop(connection);

    provider
        .handler
        .complete_owned(
            42,
            handle,
            state.clone(),
            Arc::new(TestResponder::default()),
            true,
        )
        .await
        .expect("retry frozen setup and complete Draft");

    assert!(!drafts.has_active_draft(Some(42)));
    assert!(
        persistence
            .load_all()
            .expect("reload terminal Draft envelopes")
            .is_empty()
    );
    let terminal_job = finalization
        .job(&completion_key)
        .expect("reload terminal job")
        .expect("terminal job");
    assert_eq!(
        terminal_job.stage,
        cama_db::draft_finalization::DRAFT_FINALIZATION_COMPLETE_STAGE
    );
    assert_eq!(terminal_job.pending_match_id, pending_match_id);
    let connection = Connection::open(database.path()).expect("inspect completed setup");
    assert!(
        connection
            .query_row("SELECT COUNT(*) FROM draft_financial_effects", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count applied effects")
            > 0
    );
    assert_eq!(
        provider
            .handler
            .pending
            .pending_matches(42)
            .expect("deduplicated pending matches")
            .len(),
        1
    );
}

#[tokio::test]
async fn recreated_provider_explicitly_resumes_only_linked_financial_setup() {
    let (database, provider, drafts, persistence, handle, state, _discord) =
        persistent_completion_fixture().await;
    Connection::open(database.path())
        .expect("open restart fixture")
        .execute_batch(
            "CREATE TRIGGER fail_draft_financial_setup_for_restart
             BEFORE INSERT ON draft_financial_effects
             BEGIN
                 SELECT RAISE(ABORT, 'simulated pre-restart setup failure');
             END;",
        )
        .expect("install restart setup trigger");
    provider
        .handler
        .complete_owned(
            42,
            Arc::clone(&handle),
            state.clone(),
            Arc::new(TestResponder::default()),
            true,
        )
        .await
        .expect("retain linked setup before restart");
    let lobbies = provider.handler.lobbies.clone();
    let discord = Arc::clone(&provider.handler.discord);
    Connection::open(database.path())
        .expect("reopen restart fixture")
        .execute_batch("DROP TRIGGER fail_draft_financial_setup_for_restart;")
        .expect("remove restart setup trigger");
    drop(provider);
    drop(handle);
    drop(drafts);

    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BOMB_POT_CHANCE" => Some("1".to_owned()),
        "AUTO_SPECTATOR_BET_ENABLED" => Some("true".to_owned()),
        "FIRST_GAME_POOL_DAILY_AMOUNT" => Some("999999".to_owned()),
        _ => None,
    })
    .expect("opposite restart config");
    let recreated_drafts = Arc::new(DraftStateManager::default());
    let recreated =
        DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
            database.path(),
            &config,
            lobbies,
            Arc::clone(&recreated_drafts),
            discord,
            Arc::new(NoopDraftReminderScheduler),
            Arc::new(NoopDraftNeonObserver),
            Some(persistence.clone()),
        )
        .expect("recreated provider");
    let pending = recreated
        .resume_finalizing_financial_setup(42)
        .await
        .expect("explicitly resume frozen financial setup");

    assert!(pending.state.blind_bets_result.is_some());
    assert!(!recreated_drafts.has_active_draft(Some(42)));
    let completion_key = cama_db::draft_finalization::draft_completion_key(42, state.session_id);
    let job = cama_db::draft_finalization::DraftFinalizationRepository::new(database.path())
        .job(&completion_key)
        .expect("load resumed job")
        .expect("resumed job");
    assert_eq!(
        job.stage,
        cama_db::draft_financial_execution::DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE
    );
    let envelopes = persistence
        .load_all()
        .expect("load retained finalizing envelope");
    assert_eq!(envelopes.len(), 1);
    assert!(envelopes[0].finalizing);
    assert_eq!(
        envelopes[0].pending_match_id,
        Some(pending.pending_match_id)
    );

    recreated
        .handler
        .pending
        .mutate_pending_match(42, pending.pending_match_id, |state| {
            state.origin_channel_id = Some(9_876);
            state.shuffle_message_jump_url =
                Some("https://discord.test/post-setup-publication".to_owned());
        })
        .expect("store post-setup publication metadata");
    let resumed_after_publication = recreated
        .resume_finalizing_financial_setup(42)
        .await
        .expect("setup-complete resume skips immutable executor replay");
    assert_eq!(
        resumed_after_publication.state.origin_channel_id,
        Some(9_876)
    );
    assert_eq!(
        resumed_after_publication
            .state
            .shuffle_message_jump_url
            .as_deref(),
        Some("https://discord.test/post-setup-publication")
    );
}

#[tokio::test]
async fn recreated_provider_reconciles_finalization_publication_once() {
    let (database, provider, drafts, persistence, handle, state, discord) =
        persistent_completion_fixture().await;
    let lobbies = provider.handler.lobbies.clone();
    let pending_state = db_pending_state(
        &state,
        1_700_000_000,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state");
    provider
        .handler
        .mark_finalizing(&state, None)
        .await
        .expect("fence finalization");
    let linked = provider
        .handler
        .link_finalizing_pending_match(
            &state,
            Some(&pending_state),
            Some(700),
            Some(701),
            Some(702),
        )
        .await
        .expect("link finalization plan");
    provider
        .handler
        .run_persisted_financial_setup(&state, linked.pending.pending_match_id)
        .await
        .expect("run financial setup");
    drop(handle);
    drop(drafts);
    drop(provider);

    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BOMB_POT_CHANCE" => Some("0".to_owned()),
        "AUTO_SPECTATOR_BET_ENABLED" => Some("false".to_owned()),
        "FIRST_GAME_POOL_DAILY_AMOUNT" => Some("0".to_owned()),
        _ => None,
    })
    .expect("recovery config");
    let recreated_drafts = Arc::new(DraftStateManager::default());
    let recreated =
        DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
            database.path(),
            &config,
            lobbies,
            Arc::clone(&recreated_drafts),
            discord.clone(),
            Arc::new(NoopDraftReminderScheduler),
            Arc::new(NoopDraftNeonObserver),
            Some(persistence.clone()),
        )
        .expect("recreated provider");

    let first = recreated
        .recover_finalizing()
        .await
        .expect("recover finalization");
    assert_eq!(first, 1);
    let recovered_pending = recreated
        .handler
        .pending
        .pending_match(42, linked.pending.pending_match_id)
        .expect("load recovered pending match")
        .expect("recovered pending match");
    assert_eq!(recovered_pending.state.thread_shuffle_thread_id, Some(702));
    assert_eq!(recovered_pending.state.thread_shuffle_message_id, Some(1));
    assert!(persistence.load_all().expect("load envelopes").is_empty());
    assert!(!recreated_drafts.has_active_draft(Some(42)));

    // A repeated READY/recovery pass sees the terminal CAS and performs no
    // additional Discord publication.
    assert_eq!(
        recreated
            .recover_finalizing()
            .await
            .expect("repeat recovery"),
        0
    );
    let finalization =
        cama_db::draft_finalization::DraftFinalizationRepository::new(database.path());
    assert_eq!(
        finalization
            .job(&cama_db::draft_finalization::draft_completion_key(
                42,
                state.session_id
            ))
            .expect("load terminal job")
            .expect("terminal job")
            .stage,
        cama_db::draft_finalization::DRAFT_FINALIZATION_COMPLETE_STAGE
    );
}

#[tokio::test]
async fn required_live_delivery_failure_retains_job_and_envelope() {
    let (database, provider, _drafts, persistence, _handle, state, discord) =
        persistent_completion_fixture().await;
    let pending_state = db_pending_state(
        &state,
        1_700_000_000,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state");
    provider
        .handler
        .mark_finalizing(&state, None)
        .await
        .expect("fence finalization");
    let linked = provider
        .handler
        .link_finalizing_pending_match(&state, Some(&pending_state), Some(700), Some(701), None)
        .await
        .expect("link finalization plan");
    provider
        .handler
        .run_persisted_financial_setup(&state, linked.pending.pending_match_id)
        .await
        .expect("run financial setup");
    discord.fail_delivery_sends.store(true, Ordering::Release);
    let error = provider
        .recover_finalizing()
        .await
        .expect_err("required delivery failure must remain retryable");
    assert!(error.contains("ambiguous Discord delivery failure"));
    let completion_key = cama_db::draft_finalization::draft_completion_key(42, state.session_id);
    let job = cama_db::draft_finalization::DraftFinalizationRepository::new(database.path())
        .job(&completion_key)
        .expect("load retained publication job")
        .expect("retained publication job");
    assert_eq!(
        job.stage,
        cama_db::draft_financial_execution::DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE
    );
    assert_eq!(
        persistence
            .load_all()
            .expect("load retained envelope")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_deleted_announcement_channel_is_skipped_instead_of_wedging_the_guild() {
    let (database, provider, drafts, persistence, _handle, state, discord) =
        persistent_completion_fixture().await;
    let pending_state = db_pending_state(
        &state,
        1_700_000_000,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state");
    provider
        .handler
        .mark_finalizing(&state, None)
        .await
        .expect("fence finalization");
    let linked = provider
        .handler
        .link_finalizing_pending_match(&state, Some(&pending_state), Some(700), Some(701), None)
        .await
        .expect("link finalization plan");
    provider
        .handler
        .run_persisted_financial_setup(&state, linked.pending.pending_match_id)
        .await
        .expect("run financial setup");
    // The plan froze channel 700 at link time; Discord now reports it gone, so
    // no retry can ever deliver there.
    let delivered_before = discord
        .sent
        .lock()
        .expect("sent")
        .iter()
        .filter(|(channel_id, _)| *channel_id == 700)
        .count();
    discord.undeliverable_channel.store(700, Ordering::Release);

    assert_eq!(
        provider
            .recover_finalizing()
            .await
            .expect("an undeliverable destination must not fail recovery"),
        1
    );

    assert!(
        persistence.load_all().expect("load envelopes").is_empty(),
        "the finalizing envelope must not survive; it blocks every later /draft start"
    );
    assert!(!drafts.has_active_draft(Some(42)));
    assert_eq!(
        cama_db::draft_finalization::DraftFinalizationRepository::new(database.path())
            .job(&cama_db::draft_finalization::draft_completion_key(
                42,
                state.session_id
            ))
            .expect("load terminal job")
            .expect("terminal job")
            .stage,
        cama_db::draft_finalization::DRAFT_FINALIZATION_COMPLETE_STAGE
    );
    assert_eq!(
        discord
            .sent
            .lock()
            .expect("sent")
            .iter()
            .filter(|(channel_id, _)| *channel_id == 700)
            .count(),
        delivered_before,
        "nothing new was delivered to the dead channel"
    );
}

#[tokio::test]
async fn history_cap_failure_does_not_resend_or_terminalize() {
    let (database, provider, _drafts, persistence, _handle, state, discord) =
        persistent_completion_fixture().await;
    let pending_state = db_pending_state(
        &state,
        1_700_000_000,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state");
    provider
        .handler
        .mark_finalizing(&state, None)
        .await
        .expect("fence finalization");
    let linked = provider
        .handler
        .link_finalizing_pending_match(&state, Some(&pending_state), Some(700), Some(701), None)
        .await
        .expect("link finalization plan");
    provider
        .handler
        .run_persisted_financial_setup(&state, linked.pending.pending_match_id)
        .await
        .expect("run financial setup");
    discord.fail_delivery_history.store(true, Ordering::Release);
    let sent_before = discord.sent.lock().expect("sent").len();
    assert!(provider.recover_finalizing().await.is_err());
    assert_eq!(discord.sent.lock().expect("sent").len(), sent_before);
    assert_eq!(
        persistence
            .load_all()
            .expect("load retained envelope")
            .len(),
        1
    );
    let _ = database;
}

#[tokio::test]
async fn ambiguous_accepted_send_is_reconciled_from_history_without_duplicate() {
    let (database, provider, _drafts, persistence, _handle, state, discord) =
        persistent_completion_fixture().await;
    let pending_state = db_pending_state(
        &state,
        1_700_000_000,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state");
    provider
        .handler
        .mark_finalizing(&state, None)
        .await
        .expect("fence finalization");
    let linked = provider
        .handler
        .link_finalizing_pending_match(&state, Some(&pending_state), Some(700), Some(701), None)
        .await
        .expect("link finalization plan");
    provider
        .handler
        .run_persisted_financial_setup(&state, linked.pending.pending_match_id)
        .await
        .expect("run financial setup");
    discord.fail_delivery_sends.store(true, Ordering::Release);
    discord
        .accept_delivery_before_failure
        .store(true, Ordering::Release);
    let sent_before = discord.sent.lock().expect("sent").len();
    provider
        .recover_finalizing()
        .await
        .expect("history must reconcile accepted send");
    let sent = discord.sent.lock().expect("sent");
    assert_eq!(sent.len() - sent_before, 2);
    assert_eq!(
        sent[sent_before..]
            .iter()
            .filter(|(channel_id, _)| *channel_id == 700)
            .count(),
        1
    );
    assert!(
        persistence
            .load_all()
            .expect("load terminal envelopes")
            .is_empty()
    );
    let _ = database;
}

#[tokio::test]
async fn recreated_ready_with_reminder_marker_rebuilds_timers_without_notify() {
    let (database, provider, drafts, persistence, handle, state, discord) =
        persistent_completion_fixture().await;
    let pending_state = db_pending_state(
        &state,
        1_700_000_000,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state");
    provider
        .handler
        .mark_finalizing(&state, None)
        .await
        .expect("fence finalization");
    let linked = provider
        .handler
        .link_finalizing_pending_match(&state, Some(&pending_state), Some(700), Some(701), None)
        .await
        .expect("link finalization plan");
    provider
        .handler
        .run_persisted_financial_setup(&state, linked.pending.pending_match_id)
        .await
        .expect("run financial setup");
    let completion_key = cama_db::draft_finalization::draft_completion_key(42, state.session_id);
    Connection::open(database.path())
        .expect("open reminder marker fixture")
        .execute(
            "UPDATE draft_finalization_jobs SET progress_json=?1 WHERE completion_key=?2",
            rusqlite::params![r#"{"reminders":{"scheduled":true}}"#, completion_key],
        )
        .expect("persist reminder marker");
    let lobbies = provider.handler.lobbies.clone();
    drop(handle);
    drop(drafts);
    drop(provider);

    let scheduler = Arc::new(RecoveryRecordingReminderScheduler {
        normal_calls: AtomicUsize::new(0),
        recovery_calls: AtomicUsize::new(0),
    });
    let recreated =
        DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
            database.path(),
            &ApplicationConfig::from_lookup(|name| {
                if name == "DISCORD_BOT_TOKEN" {
                    Some("test-token".to_owned())
                } else {
                    None
                }
            })
            .expect("recovery config"),
            lobbies,
            Arc::new(DraftStateManager::default()),
            discord,
            scheduler.clone(),
            Arc::new(NoopDraftNeonObserver),
            Some(persistence.clone()),
        )
        .expect("recreated provider");
    recreated
        .recover_finalizing()
        .await
        .expect("recover marked reminder job");
    assert_eq!(scheduler.normal_calls.load(Ordering::Acquire), 0);
    assert_eq!(scheduler.recovery_calls.load(Ordering::Acquire), 1);
    assert_eq!(
        recreated
            .recover_finalizing()
            .await
            .expect("repeat recovery"),
        0
    );
    assert!(
        persistence
            .load_all()
            .expect("load terminal envelopes")
            .is_empty()
    );
}

#[tokio::test]
async fn corrupt_first_guild_does_not_starve_later_finalization_recovery() {
    let (database, provider, _drafts, persistence, _handle, state, _discord) =
        persistent_completion_fixture().await;
    let pending_state = db_pending_state(
        &state,
        1_700_000_000,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("first pending state");
    provider
        .handler
        .mark_finalizing(&state, None)
        .await
        .expect("first fence");
    let first = provider
        .handler
        .link_finalizing_pending_match(&state, Some(&pending_state), Some(700), Some(701), None)
        .await
        .expect("first link");
    provider
        .handler
        .run_persisted_financial_setup(&state, first.pending.pending_match_id)
        .await
        .expect("first setup");

    let mut second_state = state.clone();
    second_state.guild_id = 43;
    second_state.session_id = state.session_id.saturating_add(1);
    let second_envelope = persistence
        .create_envelope(&second_state.to_snapshot().envelope(1))
        .expect("second envelope");
    lock_recover(&provider.handler.persisted_envelopes).insert(
        (second_state.guild_id, second_state.session_id),
        second_envelope,
    );
    provider
        .handler
        .mark_finalizing(&second_state, None)
        .await
        .expect("second fence");
    let second_pending_state = db_pending_state(
        &second_state,
        1_700_000_100,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("second pending state");
    let second = provider
        .handler
        .link_finalizing_pending_match(&second_state, Some(&second_pending_state), None, None, None)
        .await
        .expect("second link");
    Connection::open(database.path())
        .expect("open aggregate recovery fixture")
        .execute(
            "UPDATE draft_finalization_jobs SET stage='setup_complete' WHERE pending_match_id=?1",
            [second.pending.pending_match_id],
        )
        .expect("put second job at setup stage");
    Connection::open(database.path())
        .expect("open corrupt first job")
        .execute(
            "UPDATE draft_finalization_jobs SET plan_json='{}' WHERE pending_match_id=?1",
            [first.pending.pending_match_id],
        )
        .expect("corrupt first immutable plan");

    let error = provider
        .recover_finalizing()
        .await
        .expect_err("aggregate recovery reports the corrupt first guild");
    assert!(error.contains("guild 42"));
    let remaining = persistence.load_all().expect("load remaining envelopes");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].guild_id, 42);
    assert_eq!(
        cama_db::draft_finalization::DraftFinalizationRepository::new(database.path())
            .job(&cama_db::draft_finalization::draft_completion_key(
                43,
                second_state.session_id
            ))
            .expect("load second terminal job")
            .map(|job| job.stage),
        Some(cama_db::draft_finalization::DRAFT_FINALIZATION_COMPLETE_STAGE.to_owned())
    );
}

#[tokio::test]
async fn finalization_retries_uncertain_send_by_delivery_key_after_progress_crash() {
    let (database, provider, drafts, persistence, handle, state, discord) =
        persistent_completion_fixture().await;
    let lobbies = provider.handler.lobbies.clone();
    let pending_state = db_pending_state(
        &state,
        1_700_000_000,
        provider.handler.config.bet_lock_seconds,
        false,
    )
    .expect("pending state");
    provider
        .handler
        .mark_finalizing(&state, None)
        .await
        .expect("fence finalization");
    let linked = provider
        .handler
        .link_finalizing_pending_match(&state, Some(&pending_state), Some(700), Some(701), None)
        .await
        .expect("link finalization plan");
    provider
        .handler
        .run_persisted_financial_setup(&state, linked.pending.pending_match_id)
        .await
        .expect("run financial setup");
    drop(handle);
    drop(drafts);
    drop(provider);

    Connection::open(database.path())
        .expect("open progress crash fixture")
        .execute_batch(
            "CREATE TRIGGER fail_lobby_delivery_progress
             BEFORE UPDATE OF progress_json ON draft_finalization_jobs
             WHEN NEW.progress_json LIKE '%\"lobby\"%'
             BEGIN
                 SELECT RAISE(ABORT, 'simulated crash after lobby send');
             END;",
        )
        .expect("install progress crash trigger");
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BOMB_POT_CHANCE" => Some("0".to_owned()),
        "AUTO_SPECTATOR_BET_ENABLED" => Some("false".to_owned()),
        "FIRST_GAME_POOL_DAILY_AMOUNT" => Some("0".to_owned()),
        _ => None,
    })
    .expect("recovery config");
    let recreated =
        DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
            database.path(),
            &config,
            lobbies,
            Arc::new(DraftStateManager::default()),
            discord.clone(),
            Arc::new(NoopDraftReminderScheduler),
            Arc::new(NoopDraftNeonObserver),
            Some(persistence.clone()),
        )
        .expect("recreated provider");
    let lobby_sends_before_recovery = discord
        .sent
        .lock()
        .expect("sent")
        .iter()
        .filter(|(channel_id, _)| *channel_id == 700)
        .count();
    assert!(recreated.recover_finalizing().await.is_err());
    let lobby_sends_after_crash = discord
        .sent
        .lock()
        .expect("sent")
        .iter()
        .filter(|(channel_id, _)| *channel_id == 700)
        .count();
    assert_eq!(
        lobby_sends_after_crash - lobby_sends_before_recovery,
        1,
        "first pass must send the lobby copy once before the progress crash"
    );
    Connection::open(database.path())
        .expect("reopen progress crash fixture")
        .execute_batch("DROP TRIGGER fail_lobby_delivery_progress;")
        .expect("remove progress crash trigger");

    recreated
        .recover_finalizing()
        .await
        .expect("retry finalization after crash");
    let lobby_sends_after_retry = discord
        .sent
        .lock()
        .expect("sent")
        .iter()
        .filter(|(channel_id, _)| *channel_id == 700)
        .count();
    assert_eq!(
        lobby_sends_after_retry - lobby_sends_before_recovery,
        1,
        "Discord nonce must reconcile the uncertain lobby send"
    );
    assert!(persistence.load_all().expect("load envelopes").is_empty());
}

#[tokio::test]
async fn pre_job_finalizing_row_is_left_for_explicit_manual_recovery() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        _ => None,
    })
    .expect("legacy finalization config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.phase = DraftPhase::Complete;
    state.current_pick_index = DRAFT_TOTAL_PICKS;
    state.radiant_player_ids = vec![1, 3, 5, 7, 9];
    state.dire_player_ids = vec![2, 4, 6, 8, 10];
    state.radiant_hero_pick_order = Some(1);
    state.dire_hero_pick_order = Some(2);
    let mut requested = state.to_snapshot().envelope(1);
    requested.finalizing = true;
    let created = persistence
        .create_envelope(&requested)
        .expect("create pre-job fenced envelope");
    let completion_key = cama_db::draft_finalization::draft_completion_key(42, created.session_id);
    let connection = Connection::open(database.path()).expect("open pre-job fixture");
    connection
        .execute(
            "INSERT INTO pending_matches(guild_id,payload,completion_key,updated_at)
             VALUES(42,'{}',?1,CURRENT_TIMESTAMP)",
            [&completion_key],
        )
        .expect("insert pre-job pending match");
    let pending_match_id = connection.last_insert_rowid();
    drop(connection);
    let mut linked_envelope = created;
    linked_envelope.pending_match_id = Some(pending_match_id);
    let linked_envelope = persistence
        .replace_envelope_if_revision(
            42,
            linked_envelope.session_id,
            linked_envelope.revision,
            &linked_envelope,
        )
        .expect("link pre-job envelope");
    let state = linked_envelope.state.clone().into_state();
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies,
        drafts,
        discord,
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("pre-job provider");
    lock_recover(&provider.handler.persisted_envelopes)
        .insert((42, state.session_id), linked_envelope.clone());
    let pending_state = db_pending_state(&state, 100, 300, false).expect("pending state");

    let error = provider
        .handler
        .link_finalizing_pending_match(&state, Some(&pending_state), None, None, None)
        .await
        .expect_err("pre-job row must not receive a reconstructed plan");
    assert!(error.contains("finalization job") && error.contains("does not exist"));
    assert_eq!(
        persistence.load_all().expect("reload pre-job envelope"),
        vec![linked_envelope]
    );
    let connection = Connection::open(database.path()).expect("reopen pre-job fixture");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count absent jobs"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count retained pending matches"),
        1
    );
}

#[tokio::test]
async fn sqlite_persistence_constructor_rejects_split_pending_database() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let separate_persistence = migrated_database();
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        _ => None,
    })
    .expect("draft config");

    assert!(matches!(
        DraftRegistrationProvider::new_with_persistence(
            database.path(),
            &config,
            lobbies,
            drafts,
            discord,
            separate_persistence.path(),
        ),
        Err(DraftProviderBuildError::Database(message))
            if message.contains("must share one SQLite database")
    ));
}

#[tokio::test(start_paused = true)]
async fn final_pick_defer_failure_reopens_fenced_row_for_recovery() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BOMB_POT_CHANCE" => Some("0".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.phase = DraftPhase::Drafting;
    state.current_pick_index = 7;
    state.current_round_first_captain_id = Some(1);
    state.radiant_player_ids = vec![1, 3, 5, 7, 9];
    state.dire_player_ids = vec![2, 4, 6, 8];
    state.radiant_hero_pick_order = Some(1);
    state.dire_hero_pick_order = Some(2);
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    let persisted = persistence
        .create_envelope(&state.to_snapshot().envelope(1))
        .expect("create durable final-pick draft");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobbies,
        Arc::clone(&drafts),
        discord,
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");
    provider.hydrate().await.expect("hydrate final-pick draft");
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register draft");
    let handler = builder
        .build()
        .component_handler("draft")
        .expect("component handler");
    let responder = Arc::new(TestResponder {
        fail_defer: true,
        ..TestResponder::default()
    });

    let error = handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 96,
                custom_id: format!("draft:{}:pick:10", persisted.session_id),
                user_id: 2,
                user_display_name: "Captain".to_owned(),
                guild_id: Some(42),
                channel_id: Some(777),
                member_permissions: None,
                values: Vec::new(),
            },
            responder,
        )
        .await
        .expect_err("defer failure must stop completion");
    assert!(error.to_string().contains("simulated defer failure"));
    tokio::time::advance(Duration::from_secs(DRAFTING_TIMEOUT_SECONDS + 1)).await;
    tokio::task::yield_now().await;
    let fenced = persistence.load_all().expect("load fenced row");
    assert_eq!(fenced.len(), 1);
    assert!(fenced[0].finalizing);
    assert_eq!(fenced[0].pending_match_id, None);
    assert_eq!(fenced[0].state.as_state().phase, DraftPhase::Complete);
    assert_eq!(
        drafts
            .get_state(Some(42))
            .expect("fenced live state")
            .lock()
            .expect("fenced live state lock")
            .phase,
        DraftPhase::Complete
    );
    provider
        .hydrate()
        .await
        .expect("READY hydration leaves finalizing row for recovery");
    provider
        .recover_finalizing()
        .await
        .expect("final-pick fence rolls back to interactive Draft");
    let reopened = persistence
        .load_all()
        .expect("load reopened row")
        .pop()
        .expect("reopened row");
    assert!(!reopened.finalizing);
    assert_eq!(reopened.pending_match_id, None);
    assert_eq!(reopened.state.as_state().phase, DraftPhase::Drafting);
    assert!(reopened.state.as_state().current_pick_index < DRAFT_TOTAL_PICKS);
}

#[tokio::test(start_paused = true)]
async fn nonpersistent_final_pick_defer_failure_keeps_legacy_timeout_cleanup() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "BOMB_POT_CHANCE" => Some("0".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let provider = DraftRegistrationProvider::new(
        database.path(),
        &config,
        lobbies.clone(),
        Arc::clone(&drafts),
        discord,
    )
    .expect("draft provider");
    let mut state = state_for_draft();
    state.phase = DraftPhase::Drafting;
    state.current_pick_index = 7;
    state.current_round_first_captain_id = Some(1);
    state.radiant_player_ids = vec![1, 3, 5, 7, 9];
    state.dire_player_ids = vec![2, 4, 6, 8];
    state.radiant_hero_pick_order = Some(1);
    state.dire_hero_pick_order = Some(2);
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    let player_ids = state
        .player_pool_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    assert!(lobbies.reserve_players(42, AppLobbyKind::Open, &player_ids));
    drafts
        .restore_draft(state.clone())
        .expect("restore process-local draft");
    provider
        .handler
        .schedule_timeout(42, state.session_id, DRAFTING_TIMEOUT_SECONDS);
    tokio::task::yield_now().await;
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register draft");
    let handler = builder
        .build()
        .component_handler("draft")
        .expect("component handler");
    let responder = Arc::new(TestResponder {
        fail_defer: true,
        ..TestResponder::default()
    });

    let error = handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 97,
                custom_id: format!("draft:{}:pick:10", state.session_id),
                user_id: 2,
                user_display_name: "Captain".to_owned(),
                guild_id: Some(42),
                channel_id: Some(777),
                member_permissions: None,
                values: Vec::new(),
            },
            responder,
        )
        .await
        .expect_err("defer failure must stop completion");
    assert!(error.to_string().contains("simulated defer failure"));
    assert_eq!(
        drafts
            .get_state(Some(42))
            .expect("complete state before legacy timeout")
            .lock()
            .expect("complete state lock")
            .phase,
        DraftPhase::Complete
    );

    tokio::time::advance(Duration::from_secs(DRAFTING_TIMEOUT_SECONDS + 1)).await;
    tokio::task::yield_now().await;
    assert!(!drafts.has_active_draft(Some(42)));
    assert_eq!(
        Connection::open(database.path())
            .expect("open nonpersistent database")
            .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count nonpersistent finalization jobs"),
        0
    );
    assert!(lobbies.reserve_players(42, AppLobbyKind::Open, &player_ids));
    lobbies.release_players(42, AppLobbyKind::Open, &player_ids);
}

#[tokio::test]
async fn hydration_restore_failure_releases_new_reservation_and_session_lock() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let provider = DraftRegistrationProvider::new(
        database.path(),
        &config,
        lobbies.clone(),
        Arc::clone(&drafts),
        discord,
    )
    .expect("draft provider");
    drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("competing manager state");
    let mut state = state_for_draft();
    state.session_id = 99;
    let session_lock = provider.handler.draft_operation_lock(42, 99);
    let _session_guard = session_lock.lock().await;
    let lobby_lock = lobbies.operation_lock(42, AppLobbyKind::Open);
    let _lobby_guard = lobby_lock.lock().await;

    let error = provider
        .handler
        .reserve_and_restore_hydrated_state(&state, &session_lock)
        .expect_err("manager restore must lose the race");
    assert!(error.contains("already in progress"));
    assert!(
        !provider
            .handler
            .draft_operation_locks
            .lock()
            .expect("draft operation locks")
            .contains_key(&(42, 99))
    );
    let player_ids = (1..=10).collect::<BTreeSet<_>>();
    assert!(lobbies.reserve_players(42, AppLobbyKind::Open, &player_ids));
    lobbies.release_players(42, AppLobbyKind::Open, &player_ids);
}

#[test]
fn hydration_skips_unfenced_complete_state_for_recovery() {
    let database = migrated_database();
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    let mut state = state_for_draft();
    state.player_pool_ids.clear();
    state.phase = DraftPhase::Complete;
    state.draft_channel_id = Some(777);
    state.draft_message_id = Some(1234);
    persistence
        .create_envelope(&state.to_snapshot().envelope(1))
        .expect("create legacy complete draft");
    let drafts = Arc::new(DraftStateManager::default());
    let discord = Arc::new(NullDiscord::default());
    let lobby = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::new(),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 0,
        },
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("lobby provider");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobby.match_lobby_port(),
        Arc::clone(&drafts),
        discord,
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");

    assert_eq!(block_on(provider.hydrate()).expect("skip complete row"), 0);
    assert!(!drafts.has_active_draft(Some(42)));
    assert_eq!(
        persistence.load_all().expect("complete row retained").len(),
        1
    );
}

fn seed_players(path: &std::path::Path, guild_id: i64, ids: &[i64], balance: i64) {
    let mut connection = Connection::open(path).expect("open draft player fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match runtime foreign-key policy");
    let transaction = connection
        .transaction()
        .expect("begin draft player fixture transaction");
    let mut insert = transaction
        .prepare(
            "INSERT INTO players (
                 discord_id, guild_id, discord_username, preferred_roles,
                 glicko_rating, glicko_rd, glicko_volatility,
                 exclusion_count, jopacoin_balance
             ) VALUES (?1, ?2, ?3, '[\"1\"]', ?4, 100.0, 0.06, 5, ?5)",
        )
        .expect("prepare draft player fixture insert");
    for id in ids {
        insert
            .execute(rusqlite::params![
                id,
                guild_id,
                format!("P{id}"),
                1500.0 + *id as f64,
                balance,
            ])
            .expect("seed draft player");
    }
    drop(insert);
    transaction
        .commit()
        .expect("commit draft player fixture transaction");
}

#[test]
fn command_surface_is_exactly_the_four_python_subcommands() {
    let options = draft_options();
    let names = options
        .iter()
        .map(|option| option.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["start", "restart", "sampleinprogress", "samplecomplete"]
    );
    assert!(
        options[0]
            .options
            .iter()
            .any(|option| option.name == "captain1")
    );
    assert!(
        options[0]
            .options
            .iter()
            .any(|option| option.name == "captain2")
    );
    assert_eq!(
        options[0]
            .options
            .iter()
            .find(|option| option.name == "captain1")
            .expect("captain1")
            .description,
        "(Optional) Specify first captain"
    );
    assert_eq!(
        options[0]
            .options
            .iter()
            .find(|option| option.name == "captain2")
            .expect("captain2")
            .description,
        "(Optional) Specify second captain"
    );
    let lobby = options[0]
        .options
        .iter()
        .find(|option| option.name == "lobby")
        .expect("lobby choice");
    assert_eq!(lobby.choices.len(), 2);
    assert!(options[1].options.is_empty());
    assert!(options[2].options.is_empty());
    assert!(options[3].options.is_empty());
}

#[test]
fn component_parser_accepts_python_ids_and_session_ids() {
    assert_eq!(
        parse_component("draft_choice_side").expect("side").action,
        DraftComponent::ChoiceSide
    );
    assert_eq!(
        parse_component("draft_choice_hero_pick")
            .expect("hero")
            .action,
        DraftComponent::ChoiceHero
    );
    assert_eq!(
        parse_component("draft_pick_17").expect("pick").action,
        DraftComponent::Pick(17)
    );
    assert_eq!(
        parse_component("draft_pref_clear").expect("clear").action,
        DraftComponent::Preference(None)
    );
    assert_eq!(
        parse_component("draft:9:pick:17").expect("session pick"),
        ParsedComponent {
            session_id: Some(9),
            action: DraftComponent::Pick(17),
        }
    );
    assert_eq!(
        parse_component("draft:9:side:dire")
            .expect("session side")
            .action,
        DraftComponent::Side(Side::Dire)
    );
}

#[test]
fn pre_choice_views_encode_the_designated_flow() {
    let winner = pre_choice_view(9, 1, false, true);
    assert_eq!(winner[0].buttons[0].custom_id, "draft:9:choice_side");
    assert_eq!(winner[0].buttons[1].custom_id, "draft:9:choice_hero");
    let side = pre_choice_view(9, 1, true, false);
    assert_eq!(side[0].buttons[0].custom_id, "draft:9:side:radiant");
    assert_eq!(side[0].buttons[1].custom_id, "draft:9:side:dire");
    let hero = pre_choice_view(9, 2, false, false);
    assert_eq!(hero[0].buttons[0].custom_id, "draft:9:hero:first");
    assert_eq!(hero[0].buttons[1].custom_id, "draft:9:hero:second");
}

#[test]
fn drafting_view_has_eight_player_buttons_and_three_preferences() {
    let mut state = state_for_draft();
    state.phase = DraftPhase::Drafting;
    state.start_player_draft();
    let rows = drafting_view(&state);
    let buttons = rows
        .iter()
        .flat_map(|row| row.buttons.iter())
        .collect::<Vec<_>>();
    assert_eq!(buttons.len(), 11);
    assert_eq!(
        buttons[..8]
            .iter()
            .filter(|button| button.custom_id.contains(":pick:"))
            .count(),
        8
    );
    assert_eq!(buttons[8].custom_id, "draft:9:pref:radiant");
    assert_eq!(buttons[9].custom_id, "draft:9:pref:dire");
    assert_eq!(buttons[10].custom_id, "draft:9:pref:clear");
    assert!(
        buttons[..8]
            .iter()
            .all(|button| button.style == InteractionButtonStyle::Primary)
    );
}

#[test]
fn lobby_resolution_matches_python_ambiguity_and_explicit_override() {
    assert!(resolve_lobby_kind(&[], None).is_err());
    assert_eq!(
        resolve_lobby_kind(&[AppLobbyKind::Open], None),
        Ok(AppLobbyKind::Open)
    );
    assert_eq!(
        resolve_lobby_kind(
            &[AppLobbyKind::Open, AppLobbyKind::LowSkill],
            Some(AppLobbyKind::LowSkill)
        ),
        Ok(AppLobbyKind::LowSkill)
    );
    assert_eq!(
        resolve_lobby_kind(&[AppLobbyKind::Open, AppLobbyKind::LowSkill], None),
        Err(cama_app::draft::AMBIGUOUS_LOBBY_MESSAGE.to_owned())
    );
}

#[test]
fn pre_draft_state_machine_preserves_both_python_choice_orders() {
    let mut side_first = state_for_draft();
    side_first.winner_choice_type = Some("side".to_owned());
    side_first.winner_choice_value = Some("radiant".to_owned());
    side_first.loser_choice_value = Some("first".to_owned());
    complete_pre_draft(&mut side_first);
    assert_eq!(side_first.radiant_hero_pick_order, Some(2));
    assert_eq!(side_first.dire_hero_pick_order, Some(1));
    assert_eq!(side_first.phase, DraftPhase::Drafting);

    let mut hero_first = state_for_draft();
    hero_first.winner_choice_type = Some("hero_pick".to_owned());
    hero_first.winner_choice_value = Some("first".to_owned());
    hero_first.loser_choice_value = Some("dire".to_owned());
    assign_captains(&mut hero_first, 2, 1, Side::Dire);
    complete_pre_draft(&mut hero_first);
    assert_eq!(hero_first.radiant_hero_pick_order, Some(1));
    assert_eq!(hero_first.dire_hero_pick_order, Some(2));
    assert_eq!(hero_first.phase, DraftPhase::Drafting);
}

#[tokio::test]
async fn neon_coinflip_and_captain_symmetry_hooks_follow_draft_lifecycle() {
    let (database, mut provider, drafts, _discord) = provider_fixture();
    seed_players(database.path(), 42, &(1..=10).collect::<Vec<_>>(), 100);
    let neon = Arc::new(RecordingNeonObserver::default());
    Arc::get_mut(&mut provider.handler)
        .expect("exclusive draft handler")
        .neon = neon.clone();
    let handle = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("draft state");
    let players = PlayerRepository::new(database.path())
        .get_all(Some(42))
        .expect("draft players");
    provider
        .handler
        .finish_start(
            &StartContext {
                user_id: 1,
                user_display_name: "Captain".to_owned(),
                guild_id: 42,
                channel_id: 777,
                explicit_kind: None,
                captain1: None,
                captain2: None,
            },
            AppLobbyKind::Open,
            MatchLobbySnapshot {
                guild_id: 42,
                lobby_kind: AppLobbyKind::Open,
                created_by: Some(1),
                player_ids: (1..=10).collect(),
                player_join_times: BTreeMap::new(),
                confirmed_player_ids: None,
                ready_threshold: 10,
                lobby_channel_id: Some(777),
                lobby_message_id: None,
                origin_channel_id: None,
                thread_id: None,
            },
            players,
            cama_app::draft::PoolSelectionResult {
                selected_ids: (1..=10).collect(),
                excluded_ids: Vec::new(),
            },
            cama_app::draft::CaptainPair {
                captain1_id: 1,
                captain1_rating: 1_600.0,
                captain2_id: 2,
                captain2_rating: 1_600.0,
            },
            handle.clone(),
            Arc::new(TestResponder::default()),
            None,
        )
        .await
        .expect("opening with Neon hook");
    assert_eq!(
        neon.events.lock().expect("Neon events").as_slice(),
        ["coinflip"]
    );

    {
        let mut state = handle.lock().expect("state lock");
        *state = state_for_draft();
        state.phase = DraftPhase::WinnerSideChoice;
        state.captain1_rating = 1_600.0;
        state.captain2_rating = 1_600.0;
        state.coinflip_winner_id = Some(1);
        state.winner_choice_type = Some("side".to_owned());
    }
    let responder_capture = Arc::new(TestResponder::default());
    let responder: Arc<dyn InteractionResponder> = responder_capture.clone();
    provider
        .handler
        .choose_side(42, 1, handle.clone(), Side::Radiant, &responder)
        .await
        .expect("winner side choice");
    provider
        .handler
        .choose_hero(42, 2, handle, HeroOrder::First, &responder)
        .await
        .expect("loser hero choice");
    assert_eq!(
        neon.events.lock().expect("Neon events").as_slice(),
        ["coinflip", "symmetry:0"]
    );
    let responses = responder_capture.responses.lock().expect("Neon responses");
    assert_eq!(responses[0].content, "neon:symmetry");
    assert_eq!(responses[0].attachments[0].filename, "symmetry.gif");
}

#[tokio::test]
async fn neon_bomb_pot_hook_receives_persisted_pool_and_media() {
    let (database, mut provider, drafts, _discord) = provider_fixture();
    seed_players(database.path(), 42, &(1..=10).collect::<Vec<_>>(), 100);
    Connection::open(database.path())
        .expect("open seed database")
        .execute(
            "INSERT INTO nonprofit_fund(guild_id, total_collected)
             VALUES(?1, ?2)
             ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
            rusqlite::params![42, 1_000],
        )
        .expect("seed nonprofit fund");
    let neon = Arc::new(RecordingNeonObserver::default());
    let handler = Arc::get_mut(&mut provider.handler).expect("exclusive draft handler");
    handler.config.bomb_pot_chance = 1.0;
    handler.neon = neon.clone();
    let handle = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("draft state");
    let state_snapshot = {
        let mut state = handle.lock().expect("state lock");
        *state = state_for_draft();
        state.phase = DraftPhase::Complete;
        state.radiant_player_ids = vec![1, 3, 5, 7, 9];
        state.dire_player_ids = vec![2, 4, 6, 8, 10];
        state.radiant_hero_pick_order = Some(1);
        state.dire_hero_pick_order = Some(2);
        state.current_pick_index = DRAFT_TOTAL_PICKS;
        state.draft_channel_id = Some(777);
        state.draft_message_id = Some(55);
        state.clone()
    };
    let responder = Arc::new(TestResponder::default());
    provider
        .handler
        .complete_owned(42, handle, state_snapshot, responder.clone(), true)
        .await
        .expect("bomb-pot completion");
    let events = neon.events.lock().expect("Neon events").clone();
    assert_eq!(events.len(), 1);
    assert!(events[0].starts_with("bomb:"));
    let responses = responder.responses.lock().expect("Neon responses");
    assert!(responses.iter().any(|response| {
        response.content == "neon:bomb"
            && response
                .attachments
                .iter()
                .any(|attachment| attachment.filename == "bomb.gif")
    }));
}

#[test]
fn picks_and_preferences_are_atomic_and_turn_bound() {
    let mut state = state_for_draft();
    state.phase = DraftPhase::Drafting;
    state.current_round_first_captain_id = Some(1);
    assert!(!state.pick_player(3, Some(2)));
    assert!(state.set_side_preference(3, Some("radiant")));
    assert_eq!(
        state.side_preferences.get(&3).map(String::as_str),
        Some("radiant")
    );
    assert!(state.pick_player(3, Some(1)));
    assert!(!state.set_side_preference(3, Some("dire")));
}

#[test]
fn pending_state_preserves_exclusions_lock_and_draft_marker() {
    let mut state = state_for_draft();
    state.phase = DraftPhase::Complete;
    state.radiant_player_ids = vec![1, 3, 5, 7, 9];
    state.dire_player_ids = vec![2, 4, 6, 8, 10];
    state.excluded_player_ids = vec![11, 12];
    state.full_exclusion_increment_ids = vec![11];
    state.half_exclusion_increment_ids = vec![12];
    state.radiant_hero_pick_order = Some(1);
    let pending = db_pending_state(&state, 100, 300, true).expect("pending state");
    assert_eq!(pending.radiant_team_ids, state.radiant_player_ids);
    assert_eq!(pending.dire_team_ids, state.dire_player_ids);
    assert_eq!(pending.excluded_player_ids, vec![11, 12]);
    assert_eq!(pending.excluded_conditional_player_ids, vec![12]);
    assert_eq!(pending.full_exclusion_increment_ids, vec![11]);
    assert_eq!(pending.bet_lock_until, Some(400));
    assert!(pending.is_draft);
    assert!(pending.is_bomb_pot);
}

#[test]
fn sample_fixtures_match_python_negative_ids_and_lifecycle() {
    let in_progress = sample_state(42, false);
    assert_eq!(in_progress.session_id, 0);
    assert_eq!(in_progress.phase, DraftPhase::Drafting);
    assert_eq!(
        in_progress.player_pool_ids,
        vec![-101, -102, -103, -104, -105, -106, -107, -108, -109, -110]
    );
    assert_eq!(in_progress.excluded_player_ids, vec![-111, -112]);
    let complete = sample_state(42, true);
    assert_eq!(complete.phase, DraftPhase::Complete);
    assert_eq!(complete.radiant_player_ids.len(), 5);
    assert_eq!(complete.dire_player_ids.len(), 5);
}

#[test]
fn completion_error_guidance_distinguishes_durable_match() {
    let responder = Arc::new(TestResponder::default());
    block_on(completion_response(
        &(responder.clone() as Arc<dyn InteractionResponder>),
        None,
        "db".to_owned(),
        false,
    ))
    .expect("response");
    let content = responder.responses.lock().expect("responses")[0]
        .content
        .clone();
    assert!(content.contains("/draft start"));

    let responder = Arc::new(TestResponder::default());
    block_on(completion_response(
        &(responder.clone() as Arc<dyn InteractionResponder>),
        Some(99),
        "announce".to_owned(),
        true,
    ))
    .expect("followup");
    let content = responder.responses.lock().expect("responses")[0]
        .content
        .clone();
    assert!(content.contains("Match #99"));
    assert!(content.contains("/record abort"));
    assert!(!content.contains("/draft start"));
}

#[tokio::test]
async fn completion_persists_pending_seed_blinds_receipt_and_reminder() {
    let reminders = Arc::new(RecordingReminderScheduler {
        scheduled: StdMutex::new(Vec::new()),
    });
    let (database, provider, drafts, _discord) = provider_fixture_with_scheduler(reminders.clone());
    seed_players(database.path(), 42, &(1..=10).collect::<Vec<_>>(), 100);
    Connection::open(database.path())
        .expect("open seed database")
        .execute(
            "INSERT INTO nonprofit_fund(guild_id, total_collected)
             VALUES(?1, ?2)
             ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
            rusqlite::params![42, 1_000],
        )
        .expect("seed nonprofit fund");

    let handle = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("draft state");
    let (session, state_snapshot) = {
        let mut state = handle.lock().expect("state lock");
        *state = state_for_draft();
        state.phase = DraftPhase::Complete;
        state.radiant_player_ids = vec![1, 3, 5, 7, 9];
        state.dire_player_ids = vec![2, 4, 6, 8, 10];
        state.radiant_hero_pick_order = Some(1);
        state.dire_hero_pick_order = Some(2);
        state.current_pick_index = DRAFT_TOTAL_PICKS;
        state.draft_channel_id = Some(777);
        state.draft_message_id = Some(55);
        (state.session_id, state.clone())
    };
    lock_recover(&provider.handler.draft_messages).insert(
        (42, session),
        DiscordMessageReceipt {
            channel_id: 777,
            message_id: 55,
            jump_url: "https://discord.test/channels/42/777/55".to_owned(),
        },
    );

    let responder = Arc::new(TestResponder::default());
    provider
        .handler
        .complete_owned(42, handle.clone(), state_snapshot, responder.clone(), true)
        .await
        .expect("complete draft");

    assert!(!drafts.has_active_draft(Some(42)));
    assert_eq!(responder.original_edits.lock().expect("edits").len(), 1);
    let pending = PendingMatchRepository::new(database.path())
        .pending_matches(42)
        .expect("pending matches");
    assert_eq!(pending.len(), 1);
    let pending = &pending[0];
    assert!(pending.state.is_draft);
    assert!(pending.state.bet_seed_reserved > 0);
    assert!(pending.state.first_game_pool_reserved > 0);
    assert_eq!(pending.state.shuffle_channel_id, Some(777));
    assert_eq!(pending.state.shuffle_message_id, Some(55));
    assert_eq!(
        pending.state.shuffle_message_jump_url.as_deref(),
        Some("https://discord.test/channels/42/777/55")
    );
    let blind_bets = BettingServiceRepository::new(database.path())
        .get_pending_bets(
            Some(42),
            None,
            pending.state.shuffle_timestamp.expect("timestamp"),
            Some(pending.pending_match_id),
        )
        .expect("blind bets");
    assert_eq!(blind_bets.len(), 10);
    assert!(blind_bets.iter().all(|bet| bet.is_blind));
    let blind_result = pending
        .state
        .blind_bets_result
        .as_ref()
        .expect("blind result");
    assert_eq!(blind_result["created"], 10);
    let scheduled = reminders.scheduled.lock().expect("scheduled");
    assert_eq!(scheduled.len(), 1);
    assert_eq!(scheduled[0].pending_match_id, pending.pending_match_id);
    let connection = Connection::open(database.path()).expect("inspect legacy completion");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count legacy finalization jobs"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM draft_financial_effects", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count legacy financial effects"),
        0
    );
}

#[tokio::test]
async fn test_distinct_completion_channel_copies_start_together_after_edit() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("9001".to_owned()),
        "BOMB_POT_CHANCE" => Some("0".to_owned()),
        "AUTO_SPECTATOR_BET_ENABLED" => Some("false".to_owned()),
        _ => None,
    })
    .expect("config");
    let provider = DraftRegistrationProvider::new(
        database.path(),
        &config,
        lobbies.clone(),
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("draft provider");
    let selected = (1..=10).collect::<BTreeSet<_>>();
    assert!(lobbies.reserve_players(42, AppLobbyKind::Open, &selected));
    let lobby_snapshot = lobbies
        .snapshot(42, AppLobbyKind::Open)
        .expect("lobby snapshot");
    assert_eq!(lobby_snapshot.lobby_channel_id, Some(700));
    assert_eq!(lobby_snapshot.origin_channel_id, Some(777));
    let handle = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("draft state");
    let (session, state_snapshot) = {
        let mut state = handle.lock().expect("state lock");
        *state = state_for_draft();
        state.phase = DraftPhase::Complete;
        state.radiant_player_ids = vec![1, 3, 5, 7, 9];
        state.dire_player_ids = vec![2, 4, 6, 8, 10];
        state.radiant_hero_pick_order = Some(1);
        state.dire_hero_pick_order = Some(2);
        state.current_pick_index = DRAFT_TOTAL_PICKS;
        state.draft_channel_id = Some(778);
        state.draft_message_id = Some(55);
        (state.session_id, state.clone())
    };
    lock_recover(&provider.handler.draft_messages).insert(
        (42, session),
        DiscordMessageReceipt {
            channel_id: 778,
            message_id: 55,
            jump_url: "https://discord.test/channels/42/778/55".to_owned(),
        },
    );
    Connection::open(database.path())
        .expect("open seed database")
        .execute(
            "INSERT INTO nonprofit_fund(guild_id, total_collected)
             VALUES(?1, ?2)
             ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
            rusqlite::params![42, 1_000],
        )
        .expect("seed nonprofit fund");
    discord.sent.lock().expect("sent messages").clear();
    discord
        .block_completion_sends
        .store(true, Ordering::Release);
    let responder = Arc::new(TestResponder::default());
    let handler = Arc::clone(&provider.handler);
    let responder_for_task = responder.clone();
    let completion = tokio::spawn(async move {
        handler
            .complete_owned(42, handle, state_snapshot, responder_for_task, true)
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while discord.completion_sends_started.load(Ordering::Acquire) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both completion copies start concurrently");
    assert_eq!(
        responder.original_edits.lock().expect("source edits").len(),
        1,
        "the required source edit must complete before channel publication",
    );
    assert!(!completion.is_finished());
    discord.completion_send_gate.add_permits(2);
    completion
        .await
        .expect("completion task joins")
        .expect("complete draft");

    let pending = &PendingMatchRepository::new(database.path())
        .pending_matches(42)
        .expect("pending matches")[0];
    assert_eq!(pending.state.shuffle_channel_id, Some(700));
    assert_eq!(pending.state.shuffle_message_id, Some(1));
    assert_eq!(
        pending.state.shuffle_message_jump_url.as_deref(),
        Some("https://discord.test/700/1")
    );
    assert_eq!(pending.state.cmd_shuffle_channel_id, Some(777));
    assert_eq!(pending.state.cmd_shuffle_message_id, Some(1));
    let sent = discord.sent.lock().expect("sent messages");
    assert_eq!(
        sent.iter()
            .map(|(channel, _)| *channel)
            .filter(|channel| matches!(channel, 700 | 777))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([700, 777]),
    );
    assert!(
        sent.iter()
            .filter(|(channel, _)| matches!(channel, 700 | 777))
            .all(|(_, message)| {
                message.response.embeds.iter().any(|embed| {
                    embed
                        .title
                        .as_deref()
                        .is_some_and(|title| title.contains("Complete!"))
                })
            })
    );
}

#[tokio::test]
async fn completion_adds_configured_investment_and_spectator_wagers() {
    let (database, mut provider, drafts, _discord) = provider_fixture();
    {
        let handler = Arc::get_mut(&mut provider.handler).expect("exclusive draft handler");
        // 1.25% of 1,000 is 12.5; Python round uses ties-to-even => 12.
        // The old integer-percent seam would silently drop this wager.
        handler.config.auto_spectator_bet_percentage = 0.0125;
        handler.config.auto_spectator_bet_top_percentage = 0.0;
        handler.config.auto_spectator_bet_top_count = 0;
        handler.config.auto_spectator_bet_count = 1;
    }
    seed_players(database.path(), 42, &(1..=10).collect::<Vec<_>>(), 100);
    seed_players(database.path(), 42, &[99], 1_000);
    PlayerRepository::new(database.path())
        .update_balance(1, Some(42), 50)
        .expect("investment threshold balance");
    Connection::open(database.path())
        .expect("open seed database")
        .execute(
            "INSERT INTO nonprofit_fund(guild_id, total_collected)
             VALUES(?1, ?2)
             ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
            rusqlite::params![42, 1_000],
        )
        .expect("seed nonprofit fund");
    AutobetInvestmentRepository::new(database.path())
        .set(Some(42), 1, 3, "long", 10)
        .expect("configured investment");

    let handle = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("draft state");
    let (session, state_snapshot) = {
        let mut state = handle.lock().expect("state lock");
        *state = state_for_draft();
        state.phase = DraftPhase::Complete;
        state.radiant_player_ids = vec![1, 3, 5, 7, 9];
        state.dire_player_ids = vec![2, 4, 6, 8, 10];
        state.radiant_hero_pick_order = Some(1);
        state.dire_hero_pick_order = Some(2);
        state.current_pick_index = DRAFT_TOTAL_PICKS;
        state.draft_channel_id = Some(777);
        state.draft_message_id = Some(55);
        (state.session_id, state.clone())
    };
    lock_recover(&provider.handler.draft_messages).insert(
        (42, session),
        DiscordMessageReceipt {
            channel_id: 777,
            message_id: 55,
            jump_url: "https://discord.test/channels/42/777/55".to_owned(),
        },
    );

    provider
        .handler
        .complete_owned(
            42,
            handle,
            state_snapshot,
            Arc::new(TestResponder::default()),
            true,
        )
        .await
        .expect("complete draft");

    let pending = PendingMatchRepository::new(database.path())
        .pending_matches(42)
        .expect("pending matches");
    assert_eq!(pending.len(), 1);
    let pending = &pending[0];
    let result = pending
        .state
        .blind_bets_result
        .as_ref()
        .expect("automatic result");
    assert_eq!(result["created"], 10);
    assert_eq!(result["investment_bets"]["created"], 1);
    assert_eq!(result["spectator_bets"]["created"], 1);

    let bets = BettingServiceRepository::new(database.path())
        .get_pending_bets(
            Some(42),
            None,
            pending.state.shuffle_timestamp.expect("timestamp"),
            Some(pending.pending_match_id),
        )
        .expect("pending bets");
    assert_eq!(bets.len(), 12);
    assert!(bets.iter().all(|bet| bet.is_blind));
    let connection = Connection::open(database.path()).expect("open bets database");
    let attributed = connection
        .query_row(
            "SELECT investment_target_id, investment_direction
             FROM bets WHERE pending_match_id = ?1 AND investment_target_id IS NOT NULL",
            rusqlite::params![pending.pending_match_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("investment attribution");
    assert_eq!(attributed, (3, "long".to_owned()));
    let investment_amount = connection
        .query_row(
            "SELECT amount FROM bets
             WHERE pending_match_id = ?1 AND investment_target_id IS NOT NULL",
            rusqlite::params![pending.pending_match_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("investment amount");
    assert_eq!(investment_amount, 4);
    let spectator_amount = connection
        .query_row(
            "SELECT amount FROM bets
             WHERE pending_match_id = ?1 AND investment_target_id IS NULL
               AND discord_id = 99",
            rusqlite::params![pending.pending_match_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("exact spectator amount");
    assert_eq!(spectator_amount, 12);
}

#[tokio::test]
async fn test_required_edit_failure_skips_completion_channel_copies() {
    let (database, provider, drafts, discord) = provider_fixture();
    seed_players(database.path(), 42, &(1..=10).collect::<Vec<_>>(), 100);
    Connection::open(database.path())
        .expect("open seed database")
        .execute(
            "INSERT INTO nonprofit_fund(guild_id, total_collected)
             VALUES(?1, ?2)
             ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
            rusqlite::params![42, 1_000],
        )
        .expect("seed nonprofit fund");
    let handle = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("draft state");
    let state_snapshot = {
        let mut state = handle.lock().expect("state lock");
        *state = state_for_draft();
        state.phase = DraftPhase::Complete;
        state.radiant_player_ids = vec![1, 3, 5, 7, 9];
        state.dire_player_ids = vec![2, 4, 6, 8, 10];
        state.radiant_hero_pick_order = Some(1);
        state.dire_hero_pick_order = Some(2);
        state.current_pick_index = DRAFT_TOTAL_PICKS;
        state.draft_channel_id = Some(777);
        state.draft_message_id = Some(55);
        state.clone()
    };
    let responder = Arc::new(TestResponder {
        fail_original_edit: true,
        ..TestResponder::default()
    });
    provider
        .handler
        .complete_owned(42, handle, state_snapshot, responder.clone(), true)
        .await
        .expect("durable completion guidance");
    assert!(!drafts.has_active_draft(Some(42)));
    assert!(responder.original_edits.lock().expect("edits").is_empty());
    assert!(
        discord.sent.lock().expect("sent messages").is_empty(),
        "a failed required source edit must suppress all completion copies",
    );
    let responses = responder.responses.lock().expect("responses");
    assert!(responses.iter().any(|response| {
        response.content.contains("/record abort") && response.content.contains("Match #")
    }));
    assert_eq!(
        PendingMatchRepository::new(database.path())
            .pending_matches(42)
            .expect("pending matches")
            .len(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn pre_draft_and_drafting_timeouts_are_generation_owned_and_cleanup_state() {
    let (_database, provider, drafts, discord) = provider_fixture();
    let old = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("old draft");
    let old_session = old.lock().expect("old lock").session_id;
    provider
        .handler
        .schedule_timeout(42, old_session, PRE_DRAFT_TIMEOUT_SECONDS);
    tokio::task::yield_now().await;

    drafts.clear_state(Some(42), Some(&old));
    let replacement = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("replacement draft");
    let drafting = drafts
        .create_draft(Some(43), DraftLobbyKind::Open)
        .expect("drafting timeout draft");
    let drafting_session = {
        let mut state = drafting.lock().expect("drafting lock");
        state.draft_channel_id = Some(700);
        state.draft_message_id = Some(33);
        state.captain_ping_message_id = Some(44);
        state.session_id
    };
    provider
        .handler
        .schedule_timeout(43, drafting_session, DRAFTING_TIMEOUT_SECONDS);
    tokio::task::yield_now().await;
    // A stale timeout may not clear a replacement session, even after the
    // original pre-draft deadline has elapsed.
    tokio::time::advance(Duration::from_secs(PRE_DRAFT_TIMEOUT_SECONDS)).await;
    tokio::task::yield_now().await;
    assert!(Arc::ptr_eq(
        &drafts.get_state(Some(42)).expect("replacement remains"),
        &replacement
    ));

    tokio::time::advance(Duration::from_secs(
        DRAFTING_TIMEOUT_SECONDS - PRE_DRAFT_TIMEOUT_SECONDS,
    ))
    .await;
    tokio::task::yield_now().await;
    assert!(drafts.get_state(Some(43)).is_none());
    assert!(Arc::ptr_eq(
        &drafts.get_state(Some(42)).expect("replacement remains"),
        &replacement
    ));
    let edited = discord.edited.lock().expect("edited messages");
    assert!(edited.iter().any(|(channel, message, body)| {
        *channel == 700
            && *message == 33
            && body
                .response
                .embeds
                .iter()
                .any(|embed| embed.title.as_deref() == Some("⏰ Draft Timed Out"))
    }));
    assert!(
        discord
            .deleted
            .lock()
            .expect("deleted messages")
            .contains(&(700, 44))
    );
}

#[test]
fn draft_total_picks_remains_eight() {
    assert_eq!(DRAFT_TOTAL_PICKS, 8);
}

#[tokio::test]
async fn provider_registers_the_shared_manager_and_exact_route() {
    let (_database, provider, drafts, _discord) = provider_fixture();
    assert!(Arc::ptr_eq(&provider.state_manager(), &drafts));
    let mut builder = RegistryBuilder::default();
    provider
        .register(&mut builder)
        .expect("register draft provider");
    let registry = builder.build();
    assert_eq!(
        registry
            .commands()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["draft"]
    );
    assert_eq!(registry.component_routes()[0].custom_id_prefix, "draft");
}

#[tokio::test]
async fn guild_and_admin_gates_match_python_commands() {
    let (_database, provider, _drafts, _discord) = provider_fixture();
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register");
    let registry = builder.build();
    let handler = registry.command_handler("draft").expect("draft handler");

    let no_guild = Arc::new(TestResponder::default());
    handler
        .handle(
            draft_request(7, None, None, "sampleinprogress"),
            no_guild.clone(),
        )
        .await
        .expect("no-guild response");
    assert!(no_guild.responses.lock().expect("responses")[0].ephemeral);

    let non_admin = Arc::new(TestResponder::default());
    handler
        .handle(
            draft_request(7, Some(42), None, "sampleinprogress"),
            non_admin.clone(),
        )
        .await
        .expect("non-admin response");
    assert_eq!(
        non_admin.responses.lock().expect("responses")[0].content,
        "❌ Admin only command."
    );

    let admin = Arc::new(TestResponder::default());
    handler
        .handle(
            draft_request(9001, Some(42), Some(1 << 3), "sampleinprogress"),
            admin.clone(),
        )
        .await
        .expect("admin sample");
    let response = admin.responses.lock().expect("responses")[0].clone();
    assert_eq!(response.content, "**[SAMPLE UI - Not a real draft]**");
    assert!(!response.embeds.is_empty());
    assert!(!response.components.is_empty());
    let sample_ids = response
        .components
        .iter()
        .flat_map(|row| row.buttons.iter())
        .map(|button| button.custom_id.as_str())
        .collect::<Vec<_>>();
    assert!(sample_ids.iter().any(|id| id.starts_with("draft_pick_")));
    assert!(sample_ids.contains(&"draft_pref_radiant"));

    let restart = Arc::new(TestResponder::default());
    handler
        .handle(draft_request(7, Some(42), None, "restart"), restart.clone())
        .await
        .expect("restart response");
    assert_eq!(
        restart.responses.lock().expect("responses")[0].content,
        "❌ No active draft to restart."
    );
}

#[tokio::test]
async fn sample_complete_is_admin_only_and_has_no_real_manager_state() {
    let (_database, provider, drafts, _discord) = provider_fixture();
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register");
    let handler = builder.build().command_handler("draft").expect("handler");
    let responder = Arc::new(TestResponder::default());
    handler
        .handle(
            draft_request(9001, Some(42), Some(1 << 5), "samplecomplete"),
            responder.clone(),
        )
        .await
        .expect("sample complete");
    assert!(
        responder.responses.lock().expect("responses")[0]
            .content
            .contains("SAMPLE UI")
    );
    let sample = responder.responses.lock().expect("responses")[0].clone();
    let fields = &sample.embeds[0].fields;
    assert!(fields.iter().any(|field| field.name == "🎲 Blind Bets"));
    assert!(
        fields
            .iter()
            .any(|field| field.value.contains("Auto-liquidity"))
    );
    assert!(!drafts.has_active_draft(Some(42)));
}

#[tokio::test]
async fn component_wrong_user_is_rejected_and_winner_choice_deletes_ping() {
    let (_database, provider, drafts, _discord) = provider_fixture();
    let state = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("state");
    {
        let mut state = state.lock().expect("state lock");
        state.captain1_id = Some(10);
        state.captain2_id = Some(11);
        state.coinflip_winner_id = Some(10);
        state.phase = DraftPhase::WinnerChoice;
        state.draft_channel_id = Some(777);
        state.captain_ping_message_id = Some(1);
    }
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register");
    let handler = builder
        .build()
        .component_handler("draft_choice_side")
        .expect("route");
    let wrong = Arc::new(TestResponder::default());
    handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 1,
                custom_id: "draft_choice_side".to_owned(),
                user_id: 11,
                user_display_name: "Loser".to_owned(),
                guild_id: Some(42),
                channel_id: Some(777),
                member_permissions: None,
                values: Vec::new(),
            },
            wrong.clone(),
        )
        .await
        .expect("wrong user response");
    assert_eq!(
        wrong.responses.lock().expect("responses")[0].content,
        "Only the coinflip winner can make this choice."
    );
    assert_eq!(
        state.lock().expect("state lock").phase,
        DraftPhase::WinnerChoice
    );

    let winner = Arc::new(TestResponder::default());
    handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 2,
                custom_id: "draft_choice_side".to_owned(),
                user_id: 10,
                user_display_name: "Captain".to_owned(),
                guild_id: Some(42),
                channel_id: Some(777),
                member_permissions: None,
                values: Vec::new(),
            },
            winner.clone(),
        )
        .await
        .expect("winner response");
    assert_eq!(
        state.lock().expect("state lock").phase,
        DraftPhase::WinnerSideChoice
    );
    assert_eq!(winner.updates.lock().expect("updates").len(), 1);
}

#[tokio::test]
async fn restart_is_captain_or_admin_only_and_preserves_pending_matches() {
    let (_database, provider, drafts, _discord) = provider_fixture();
    let state = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("state");
    {
        let mut state = state.lock().expect("state lock");
        state.captain1_id = Some(10);
        state.captain2_id = Some(11);
        state.player_pool_ids = (1..=10).collect();
        state.phase = DraftPhase::WinnerChoice;
    }
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register");
    let handler = builder.build().command_handler("draft").expect("handler");

    let unauthorized = Arc::new(TestResponder::default());
    handler
        .handle(
            draft_request(12, Some(42), None, "restart"),
            unauthorized.clone(),
        )
        .await
        .expect("unauthorized response");
    assert_eq!(
        unauthorized.responses.lock().expect("responses")[0].content,
        "❌ Only captains or server admins can restart the draft."
    );
    assert!(drafts.has_active_draft(Some(42)));

    state.lock().expect("state lock").phase = DraftPhase::Complete;
    let finalizing = Arc::new(TestResponder::default());
    handler
        .handle(
            draft_request(10, Some(42), None, "restart"),
            finalizing.clone(),
        )
        .await
        .expect("finalizing response");
    assert_eq!(
        finalizing.responses.lock().expect("responses")[0].content,
        "⏳ Draft results are still being finalized. Please wait."
    );
    assert!(drafts.has_active_draft(Some(42)));

    state.lock().expect("state lock").phase = DraftPhase::Drafting;
    let captain = Arc::new(TestResponder::default());
    handler
        .handle(
            draft_request(10, Some(42), None, "restart"),
            captain.clone(),
        )
        .await
        .expect("restart response");
    assert!(!drafts.has_active_draft(Some(42)));
    assert!(
        captain.responses.lock().expect("responses")[0]
            .content
            .contains("Draft Restarted")
    );
}

#[tokio::test]
async fn start_reserves_the_shared_lobby_and_restart_releases_only_that_pool() {
    let (database, _lobby, lobbies, drafts, discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("9001".to_owned()),
        _ => None,
    })
    .expect("config");
    let provider = DraftRegistrationProvider::new(
        database.path(),
        &config,
        lobbies.clone(),
        Arc::clone(&drafts),
        discord,
    )
    .expect("draft provider");
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register draft");
    let registry = builder.build();
    let handler = registry.command_handler("draft").expect("draft handler");
    let started = Arc::new(TestResponder::default());
    handler
        .handle(draft_request(1, Some(42), None, "start"), started.clone())
        .await
        .expect("start draft");

    let state = drafts.get_state(Some(42)).expect("active draft");
    let snapshot = state.lock().expect("state lock").clone();
    assert_eq!(snapshot.player_pool_ids.len(), DRAFT_POOL_SIZE);
    let selected = snapshot
        .player_pool_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    assert!(!lobbies.reserve_players(42, AppLobbyKind::Open, &selected));
    assert_eq!(started.defers.lock().expect("defers").clone(), vec![false]);

    let restarted = Arc::new(TestResponder::default());
    handler
        .handle(
            draft_request(1, Some(42), None, "restart"),
            restarted.clone(),
        )
        .await
        .expect("restart draft");
    assert!(!drafts.has_active_draft(Some(42)));
    assert!(lobbies.reserve_players(42, AppLobbyKind::Open, &selected));
    lobbies.release_players(42, AppLobbyKind::Open, &selected);
    assert_eq!(
        lobbies
            .snapshot(42, AppLobbyKind::Open)
            .expect("lobby preserved")
            .player_ids
            .len(),
        10
    );
}

#[tokio::test]
async fn discord_opening_failure_unwinds_state_and_lobby_reservation() {
    let (database, _lobby, lobbies, drafts, _discord) = populated_lobby_fixture().await;
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        _ => None,
    })
    .expect("config");
    let provider = DraftRegistrationProvider::new(
        database.path(),
        &config,
        lobbies.clone(),
        Arc::clone(&drafts),
        Arc::new(FailingSendDiscord),
    )
    .expect("draft provider");
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register draft");
    let handler = builder.build().command_handler("draft").expect("handler");
    let responder = Arc::new(TestResponder::default());
    assert!(
        handler
            .handle(draft_request(1, Some(42), None, "start"), responder.clone(),)
            .await
            .is_err()
    );
    assert!(!drafts.has_active_draft(Some(42)));

    let all_ids = (1..=10).collect::<BTreeSet<_>>();
    assert!(lobbies.reserve_players(42, AppLobbyKind::Open, &all_ids));
    lobbies.release_players(42, AppLobbyKind::Open, &all_ids);
}

#[tokio::test]
async fn dynamic_pick_and_preference_callbacks_mutate_only_shared_state() {
    let (_database, provider, drafts, _discord) = provider_fixture();
    let state = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("state");
    {
        let mut state = state.lock().expect("state lock");
        *state = state_for_draft();
        state.phase = DraftPhase::Drafting;
        state.current_round_first_captain_id = Some(1);
    }
    let session = state.lock().expect("state lock").session_id;
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register");
    let handler = builder
        .build()
        .component_handler(&format!("draft:{session}:pick:3"))
        .expect("route");
    let preference = Arc::new(TestResponder::default());
    handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 3,
                custom_id: format!("draft:{session}:pref:radiant"),
                user_id: 3,
                user_display_name: "Player".to_owned(),
                guild_id: Some(42),
                channel_id: Some(777),
                member_permissions: None,
                values: Vec::new(),
            },
            preference.clone(),
        )
        .await
        .expect("preference");
    assert_eq!(
        state
            .lock()
            .expect("state lock")
            .side_preferences
            .get(&3)
            .map(String::as_str),
        Some("radiant")
    );

    let pick = Arc::new(TestResponder::default());
    handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 4,
                custom_id: format!("draft:{session}:pick:3"),
                user_id: 1,
                user_display_name: "Captain".to_owned(),
                guild_id: Some(42),
                channel_id: Some(777),
                member_permissions: None,
                values: Vec::new(),
            },
            pick.clone(),
        )
        .await
        .expect("pick");
    assert!(
        state
            .lock()
            .expect("state lock")
            .radiant_player_ids
            .contains(&3)
    );
    assert!(!pick.updates.lock().expect("updates").is_empty());
}

#[tokio::test]
async fn drafting_components_reject_nonparticipants_before_mutation() {
    let (_database, provider, drafts, _discord) = provider_fixture();
    let state = drafts
        .create_draft(Some(42), DraftLobbyKind::Open)
        .expect("state");
    {
        let mut state = state.lock().expect("state lock");
        *state = state_for_draft();
        state.phase = DraftPhase::Drafting;
    }
    let session = state.lock().expect("state lock").session_id;
    let mut builder = RegistryBuilder::default();
    provider.register(&mut builder).expect("register");
    let handler = builder
        .build()
        .component_handler(&format!("draft:{session}:pref:radiant"))
        .expect("route");
    let responder = Arc::new(TestResponder::default());
    handler
        .handle(
            InteractionRequest::Component {
                interaction_id: 5,
                custom_id: format!("draft:{session}:pref:radiant"),
                user_id: 999,
                user_display_name: "Outsider".to_owned(),
                guild_id: Some(42),
                channel_id: Some(777),
                member_permissions: None,
                values: Vec::new(),
            },
            responder.clone(),
        )
        .await
        .expect("nonparticipant response");
    assert_eq!(
        responder.responses.lock().expect("responses")[0].content,
        "❌ You are not part of this draft."
    );
    assert!(
        !state
            .lock()
            .expect("state lock")
            .side_preferences
            .contains_key(&999)
    );
}

#[test]
fn one_guild_hydration_failure_does_not_block_other_guilds() {
    // Recovery runs across every persisted draft. A guild whose draft message
    // was deleted while the bot was down fails its repaint; that must not leave
    // every other guild unhydrated -- and, because ready recovery gives up on
    // the first error, unfinalized as well.
    let database = migrated_database();
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("9001".to_owned()),
        _ => None,
    })
    .expect("draft config");
    let persistence = Arc::new(SqliteDraftStatePersistence::new(database.path()));
    for (guild_id, channel_id) in [(42_i64, 777_u64), (43, 778)] {
        let mut state = DraftState::with_session(guild_id, DraftLobbyKind::Open, 9);
        state.captain1_id = Some(1);
        state.captain2_id = Some(2);
        state.radiant_captain_id = Some(1);
        state.dire_captain_id = Some(2);
        state.coinflip_winner_id = Some(1);
        state.phase = DraftPhase::WinnerSideChoice;
        state.draft_channel_id = Some(i64::try_from(channel_id).expect("channel"));
        state.draft_message_id = Some(1234);
        persistence
            .create_envelope(&state.to_snapshot().envelope(1))
            .expect("create durable draft");
    }

    let drafts = Arc::new(DraftStateManager::default());
    let discord = Arc::new(NullDiscord::default());
    // Only the first guild's repaint fails.
    discord.fail_edits_for_channel.store(777, Ordering::Release);
    let lobby = LobbyRegistrationProvider::new(
        database.path(),
        LobbyRuntimeConfig {
            lobby_channel_id: Some(700),
            low_skill_lobby_channel_id: Some(701),
            admin_user_ids: BTreeSet::from([9001]),
            ready_threshold: 10,
            max_players: 20,
            first_game_pool_daily_amount: 0,
        },
        Arc::clone(&drafts),
        discord.clone(),
    )
    .expect("lobby provider");
    let provider = DraftRegistrationProvider::new_with_reminder_scheduler_and_neon_and_persistence(
        database.path(),
        &config,
        lobby.match_lobby_port(),
        Arc::clone(&drafts),
        discord.clone(),
        Arc::new(NoopDraftReminderScheduler),
        Arc::new(NoopDraftNeonObserver),
        Some(persistence.clone()),
    )
    .expect("draft provider");

    let error = block_on(provider.hydrate()).expect_err("the broken guild is reported");
    assert!(error.contains("guild 42"), "unexpected report: {error}");
    assert!(
        drafts.has_active_draft(Some(43)),
        "the healthy guild must still hydrate"
    );
}
