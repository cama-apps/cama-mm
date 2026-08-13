use std::collections::BTreeSet;

use cama_app::reminders::ReminderClock;
use cama_db::schema_manager::initialize_or_migrate;
use rusqlite::Connection;
use tempfile::NamedTempFile;

use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessageReceipt, DiscordMessageSnapshot,
};
use crate::gateway_events::{GatewayMember, GuildMemberPageSource};
use crate::registration::{InteractionResponseError, RegistryBuilder};

use super::*;

const GUILD: i64 = 707;
const USER: i64 = 808;

fn migrated_database() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(file.path()).expect("migrate database");
    file
}

pub(crate) fn test_provider(
    path: &Path,
    discord: Arc<dyn DiscordTransport>,
    cooldowns: ReminderCooldowns,
) -> ReminderRegistrationProvider {
    let service = ReminderService::new(
        SqliteReminderStore::new(path, 20),
        SystemReminderClock,
        RuntimeDiscordPort,
    )
    .with_cooldowns(cooldowns);
    ReminderRegistrationProvider::from_service(service, discord)
}

#[derive(Default)]
pub(crate) struct MockDiscord {
    dms: Mutex<Vec<(u64, DiscordMessage)>>,
    fail_users: Mutex<BTreeSet<u64>>,
}

impl MockDiscord {
    fn fail_for(&self, user_id: u64) {
        self.fail_users.lock().expect("failures").insert(user_id);
    }

    fn dms(&self) -> Vec<(u64, DiscordMessage)> {
        self.dms.lock().expect("dms").clone()
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
        _message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        Ok(DiscordMessageReceipt {
            channel_id,
            message_id: 1,
            jump_url: String::new(),
        })
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
        Ok(1)
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
        self.dms.lock().expect("dms").push((user_id, message));
        if self.fail_users.lock().expect("failures").contains(&user_id) {
            Err("Forbidden".to_owned())
        } else {
            Ok(())
        }
    }

    async fn guild_member(
        &self,
        _guild_id: u64,
        _user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        Ok(None)
    }
}

#[derive(Default)]
struct RecordingResponder {
    responses: Mutex<Vec<InteractionResponse>>,
    updates: Mutex<Vec<InteractionResponse>>,
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
        panic!("reminder recovery does not fetch guild members")
    }
}

fn command_request() -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "setreminder".to_owned(),
        user_id: USER as u64,
        user_display_name: "Reminder User".to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(909),
        member_permissions: None,
        options: Vec::new(),
    }
}

fn component_request(custom_id: &str) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: 2,
        custom_id: custom_id.to_owned(),
        user_id: USER as u64,
        user_display_name: "Reminder User".to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(909),
        member_permissions: None,
        values: Vec::new(),
    }
}

#[tokio::test]
async fn setreminder_registers_exact_persistent_five_button_ui_and_persists_toggle() {
    let file = migrated_database();
    let discord = Arc::new(MockDiscord::default());
    let provider = test_provider(file.path(), discord, ReminderCooldowns::default());
    let mut builder = RegistryBuilder::default();
    builder.add_provider(&provider).expect("register provider");
    let registry = builder.build();
    let command = registry
        .commands()
        .find(|command| command.name == "setreminder")
        .expect("setreminder command");
    assert_eq!(
        command.description,
        "Configure DM reminders for cooldowns and match betting windows."
    );
    assert!(command.options.is_empty());
    assert_eq!(registry.component_routes()[0].custom_id_prefix, "reminder_");

    let responder = Arc::new(RecordingResponder::default());
    command
        .handler
        .handle(command_request(), responder.clone())
        .await
        .expect("render preferences");
    let response = responder.response();
    assert!(response.ephemeral);
    assert_eq!(
        response.content,
        "**Your reminder settings:**\n\n\
         🔕 **Gamba/Wheel** — DM when your wheel cooldown expires\n\
         🔕 **Trivia** — DM when your trivia cooldown expires\n\
         🔕 **Betting** — DM when a new match opens for betting\n\
         🔕 **Dig** — DM when your free dig cooldown expires\n\
         🔕 **Pet** — DM when your cama gets dangerously hungry (and when it dies)"
    );
    let buttons = &response.components[0].buttons;
    assert_eq!(buttons.len(), 5);
    assert_eq!(
        buttons
            .iter()
            .map(|button| (
                button.custom_id.as_str(),
                button.label.as_str(),
                button.emoji.as_deref(),
                button.style,
            ))
            .collect::<Vec<_>>(),
        [
            (
                "reminder_wheel",
                "Gamba/Wheel: OFF",
                Some("🔕"),
                InteractionButtonStyle::Secondary,
            ),
            (
                "reminder_trivia",
                "Trivia: OFF",
                Some("🔕"),
                InteractionButtonStyle::Secondary,
            ),
            (
                "reminder_betting",
                "Betting: OFF",
                Some("🔕"),
                InteractionButtonStyle::Secondary,
            ),
            (
                "reminder_dig",
                "Dig: OFF",
                Some("🔕"),
                InteractionButtonStyle::Secondary,
            ),
            (
                "reminder_pet",
                "Pet: OFF",
                Some("🔕"),
                InteractionButtonStyle::Secondary,
            ),
        ]
    );

    let toggle_responder = Arc::new(RecordingResponder::default());
    registry
        .component_handler("reminder_wheel")
        .expect("persistent component route")
        .handle(
            component_request("reminder_wheel"),
            toggle_responder.clone(),
        )
        .await
        .expect("toggle wheel");
    let update = toggle_responder.update_response();
    assert_eq!(update.components[0].buttons[0].label, "Gamba/Wheel: ON");
    assert_eq!(
        update.components[0].buttons[0].style,
        InteractionButtonStyle::Success
    );

    // A fresh provider reconstructs the status solely from SQLite, proving the
    // static component route and preference survive a process restart.
    let restarted = test_provider(
        file.path(),
        Arc::new(MockDiscord::default()),
        ReminderCooldowns::default(),
    );
    let preferences = restarted
        .hooks
        .state
        .preferences(UserId::new(USER), GuildId::new(GUILD))
        .await
        .expect("persisted preferences");
    assert!(preferences.enabled(ReminderKind::Wheel));
}

#[tokio::test]
async fn ready_recovery_isolates_invalid_guild_and_failed_dm_is_consumed_once() {
    let file = migrated_database();
    let now = SystemReminderClock.now_seconds();
    let connection = Connection::open(file.path()).expect("open database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match runtime SQLite compatibility mode");
    connection
        .execute(
            "INSERT INTO players
                (discord_id,guild_id,discord_username,last_wheel_spin)
             VALUES (?1,?2,'recovery-user',?3)",
            (USER, GUILD, now),
        )
        .expect("seed timestamp");
    connection
        .execute(
            "INSERT INTO reminder_preferences
                (discord_id,guild_id,wheel_enabled,updated_at)
             VALUES (?1,?2,1,?3)",
            (USER, GUILD, now),
        )
        .expect("seed preference");
    drop(connection);

    let discord = Arc::new(MockDiscord::default());
    discord.fail_for(USER as u64);
    let provider = test_provider(
        file.path(),
        discord.clone(),
        ReminderCooldowns {
            wheel_seconds: 10_000,
            trivia_seconds: 10_000,
        },
    );
    let context = ReadyRecoveryContext::new(
        Arc::<[u64]>::from([GUILD as u64, u64::MAX]),
        Arc::new(UnusedMembers),
    );
    let report = provider.observer.ready_recovery(context).await;
    assert_eq!(report.guilds_attempted, 2);
    assert_eq!(report.guilds_refreshed, 1);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].guild_id, u64::MAX);

    let key = ReminderTaskKey::new(UserId::new(USER), GuildId::new(GUILD), ReminderKind::Wheel);
    let task = provider
        .hooks
        .state
        .task_snapshot(key)
        .expect("task snapshot")
        .expect("recovered wheel task");
    Arc::clone(&provider.hooks.state)
        .fire_task(key, task.id)
        .await;
    Arc::clone(&provider.hooks.state)
        .fire_task(key, task.id)
        .await;
    let dms = discord.dms();
    assert_eq!(dms.len(), 1, "failed scheduled DM is not retried");
    assert_eq!(dms[0].0, USER as u64);
    assert_eq!(
        dms[0].1.response.content,
        "Your wheel cooldown has expired! You can `/gamba` again now."
    );
    assert!(
        provider
            .hooks
            .state
            .task_snapshot(key)
            .expect("task snapshot")
            .is_none()
    );
}

// tests/test_dig_reminder_command.py::test_schedule_dig_reminder_delegates_to_reconciliation
#[tokio::test]
async fn typed_hooks_reconcile_and_cancel_dig_and_deliver_betting_with_isolation() {
    let file = migrated_database();
    let now = SystemReminderClock.now_seconds();
    let connection = Connection::open(file.path()).expect("open database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match runtime SQLite compatibility mode");
    connection
        .execute(
            "INSERT INTO tunnels (discord_id,guild_id,last_dig_at)
             VALUES (?1,?2,?3)",
            (USER, GUILD, now),
        )
        .expect("seed tunnel");
    connection
        .execute(
            "INSERT INTO reminder_preferences
                (discord_id,guild_id,wheel_enabled,trivia_enabled,betting_enabled,
                 dig_enabled,pet_enabled,updated_at)
             VALUES (?1,?2,1,1,1,1,1,?3)",
            (USER, GUILD, now),
        )
        .expect("seed primary preferences");
    connection
        .execute(
            "INSERT INTO reminder_preferences
                (discord_id,guild_id,betting_enabled,updated_at)
             VALUES (?1,?2,1,?3)",
            (USER + 1, GUILD, now),
        )
        .expect("seed failing subscriber preference");
    connection
        .execute(
            "INSERT INTO pets
                (discord_id,guild_id,name,species,adopted_at,hatched_at,adopt_fee,
                 last_fed_at,hunger_at_last_fed,dig_work_at)
             VALUES (?1,?2,'Alpaca','common_cama',?3,?3,0,?3,100,?3)",
            (USER, GUILD, now),
        )
        .expect("seed living pet");
    drop(connection);

    let discord = Arc::new(MockDiscord::default());
    discord.fail_for((USER + 1) as u64);
    let provider = test_provider(file.path(), discord.clone(), ReminderCooldowns::default());
    let hooks = provider.hooks();
    hooks
        .reconcile_dig(USER, GUILD, Some(now))
        .await
        .expect("reconcile dig");
    let key = ReminderTaskKey::new(UserId::new(USER), GuildId::new(GUILD), ReminderKind::Dig);
    assert!(
        provider
            .hooks
            .state
            .task_snapshot(key)
            .expect("task snapshot")
            .is_some()
    );
    assert_eq!(
        provider
            .hooks
            .state
            .task_snapshot(key)
            .expect("task snapshot")
            .expect("scheduled dig reminder")
            .due_at,
        now + cama_app::dig_service::FREE_DIG_COOLDOWN_SECONDS,
        "the hook preserves the authoritative ready timestamp"
    );
    hooks.cancel_dig(USER, GUILD).expect("cancel dig");
    assert!(
        provider
            .hooks
            .state
            .task_snapshot(key)
            .expect("task snapshot")
            .is_none()
    );

    hooks
        .schedule_wheel(USER, GUILD, now + 10_000)
        .await
        .expect("schedule wheel");
    hooks
        .schedule_trivia(USER, GUILD, now + 10_000)
        .await
        .expect("schedule trivia");
    assert!(
        hooks
            .pet_enabled(USER, GUILD)
            .await
            .expect("read durable pet preference")
    );
    hooks.rearm_pet(USER, GUILD).await.expect("rearm pet");
    for kind in [ReminderKind::Wheel, ReminderKind::Trivia, ReminderKind::Pet] {
        assert!(
            provider
                .hooks
                .state
                .task_snapshot(ReminderTaskKey::new(
                    UserId::new(USER),
                    GuildId::new(GUILD),
                    kind,
                ))
                .expect("task snapshot")
                .is_some(),
            "{kind} hook should arm a task"
        );
    }
    hooks
        .cancel_pet_async(USER, GUILD)
        .await
        .expect("cancel pet off the async worker thread");
    assert!(
        provider
            .hooks
            .state
            .task_snapshot(ReminderTaskKey::new(
                UserId::new(USER),
                GuildId::new(GUILD),
                ReminderKind::Pet,
            ))
            .expect("pet task snapshot")
            .is_none()
    );

    // Keep enough sub-minute slack that the policy's whole-minute display is
    // deterministic even if the system clock advances during the assertion.
    let bet_lock_until = SystemReminderClock.now_seconds() + 619;
    let report = hooks
        .notify_betting(GUILD, bet_lock_until, Some(55), Some(LobbyKind::LowSkill))
        .await
        .expect("notify betting");
    assert_eq!(report.attempted, 2);
    assert_eq!(report.delivered, 1);
    assert_eq!(report.failures.len(), 1);
    let dms = discord.dms();
    assert_eq!(dms.len(), 2);
    assert_eq!(
        dms[0].1.response.content,
        "A new 🧀 Whine & Cheese match (Match #55) has been shuffled! Betting is open for ~10 more minutes. Use `/bet` now!"
    );
}

// tests/test_dig_reminder_command.py::test_schedule_dig_reminder_reuses_ready_timestamp
#[tokio::test]
async fn dig_reconciliation_reuses_the_authoritative_ready_timestamp() {
    let file = migrated_database();
    let now = SystemReminderClock.now_seconds();
    let connection = Connection::open(file.path()).expect("open timestamp database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match reminder SQLite compatibility mode");
    connection
        .execute(
            "INSERT INTO tunnels (discord_id,guild_id,last_dig_at)
             VALUES (?1,?2,?3)",
            (USER, GUILD, now),
        )
        .expect("seed timestamp tunnel");
    connection
        .execute(
            "INSERT INTO reminder_preferences
                (discord_id,guild_id,dig_enabled,updated_at)
             VALUES (?1,?2,1,?3)",
            (USER, GUILD, now),
        )
        .expect("enable timestamp reminder");
    drop(connection);

    let provider = test_provider(
        file.path(),
        Arc::new(MockDiscord::default()),
        ReminderCooldowns::default(),
    );
    provider
        .hooks()
        .reconcile_dig(USER, GUILD, Some(now))
        .await
        .expect("reconcile timestamp reminder");
    let task = provider
        .hooks
        .state
        .task_snapshot(ReminderTaskKey::new(
            UserId::new(USER),
            GuildId::new(GUILD),
            ReminderKind::Dig,
        ))
        .expect("timestamp task snapshot")
        .expect("authoritative ready task");
    assert_eq!(
        task.due_at,
        now + cama_app::dig_service::FREE_DIG_COOLDOWN_SECONDS
    );
}
