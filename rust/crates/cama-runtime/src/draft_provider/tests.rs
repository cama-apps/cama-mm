use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Semaphore;

use cama_app::draft::{DRAFT_POOL_SIZE, DRAFT_TOTAL_PICKS};
use cama_db::autobet_investments::AutobetInvestmentRepository;
use cama_db::core_repositories::{NewPlayer, PlayerRepository};
use cama_db::schema_manager::initialize_or_migrate;
use rusqlite::Connection;
use tempfile::NamedTempFile;

use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessage, DiscordMessageReceipt,
    DiscordMessageSnapshot, DiscordTransport,
};
use crate::lobby_provider::{LobbyRegistrationProvider, LobbyRuntimeConfig};
use crate::registration::{
    InteractionAttachment, InteractionOption, InteractionRequest, InteractionResponseError,
    InteractionValue,
};

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
    fail_original_edit: bool,
}

#[async_trait]
impl InteractionResponder for TestResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.responses.lock().expect("responses").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
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
    edited: Arc<StdMutex<Vec<(u64, u64, DiscordMessage)>>>,
    deleted: Arc<StdMutex<Vec<(u64, u64)>>>,
    block_completion_sends: AtomicBool,
    completion_sends_started: AtomicUsize,
    completion_send_gate: Semaphore,
}

impl Default for NullDiscord {
    fn default() -> Self {
        Self {
            sent: Arc::default(),
            edited: Arc::default(),
            deleted: Arc::default(),
            block_completion_sends: AtomicBool::new(false),
            completion_sends_started: AtomicUsize::new(0),
            completion_send_gate: Semaphore::new(0),
        }
    }
}

#[async_trait]
impl DiscordTransport for NullDiscord {
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

    async fn edit_message(
        &self,
        channel_id: u64,
        message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
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
    NamedTempFile,
    DraftRegistrationProvider,
    Arc<DraftStateManager>,
    Arc<NullDiscord>,
) {
    let database = NamedTempFile::new().expect("draft database");
    initialize_or_migrate(database.path()).expect("migrate draft database");
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
    NamedTempFile,
    DraftRegistrationProvider,
    Arc<DraftStateManager>,
    Arc<NullDiscord>,
) {
    let database = NamedTempFile::new().expect("draft database");
    initialize_or_migrate(database.path()).expect("migrate draft database");
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
    NamedTempFile,
    LobbyRegistrationProvider,
    MatchLobbyPort,
    Arc<DraftStateManager>,
    Arc<NullDiscord>,
) {
    let database = NamedTempFile::new().expect("lobby database");
    initialize_or_migrate(database.path()).expect("migrate lobby database");
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
    for user_id in 2..=10 {
        handler
            .handle(
                lobby_request(user_id, "join"),
                Arc::new(TestResponder::default()),
            )
            .await
            .expect("join lobby");
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

fn seed_players(path: &std::path::Path, guild_id: i64, ids: &[i64], balance: i64) {
    let repository = PlayerRepository::new(path);
    for id in ids {
        let mut player = NewPlayer::new(*id, format!("P{id}"), Some(guild_id));
        player.preferred_roles = Some(vec!["1".to_owned()]);
        player.glicko_rating = Some(1500.0 + *id as f64);
        player.glicko_rd = Some(100.0);
        player.glicko_volatility = Some(0.06);
        repository.add(&player).expect("seed player");
        repository
            .update_balance(*id, Some(guild_id), balance)
            .expect("seed player balance");
    }
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
    tokio::time::timeout(Duration::from_secs(1), async {
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
