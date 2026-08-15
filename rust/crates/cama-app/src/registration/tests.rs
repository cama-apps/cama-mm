use std::cell::RefCell;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::rc::Rc;

use super::*;

const GUILD: i64 = 42_424;

#[test]
fn test_constructor_signature_is_bot_and_player_service_only() {
    let commands = RegistrationCommands::new("bot", "player-service");
    assert_eq!(commands.bot, "bot");
    assert_eq!(commands.player_service, "player-service");
}

#[derive(Clone, Debug)]
struct FakeReminder {
    trace: Rc<RefCell<Vec<&'static str>>>,
    preference: Option<(i64, i64, bool)>,
    duplicate: Result<bool, ReminderPortError>,
    create: Result<bool, ReminderPortError>,
}

impl Default for FakeReminder {
    fn default() -> Self {
        Self {
            trace: Rc::new(RefCell::new(Vec::new())),
            preference: None,
            duplicate: Ok(false),
            create: Ok(true),
        }
    }
}

impl LobbyReminderPort for FakeReminder {
    fn set_preference(
        &mut self,
        subscriber_id: i64,
        guild_id: i64,
        enabled: bool,
    ) -> Result<(), ReminderPortError> {
        self.trace.borrow_mut().push("preference");
        self.preference = Some((subscriber_id, guild_id, enabled));
        Ok(())
    }

    fn has_player_subscription(
        &mut self,
        _subscriber_id: i64,
        _target_id: i64,
        _guild_id: i64,
    ) -> Result<bool, ReminderPortError> {
        self.trace.borrow_mut().push("dup_check");
        self.duplicate
    }

    fn add_player_subscription(
        &mut self,
        _subscriber_id: i64,
        _target_id: i64,
        _guild_id: i64,
    ) -> Result<bool, ReminderPortError> {
        self.trace.borrow_mut().push("persist");
        self.create
    }
}

#[derive(Clone, Debug)]
struct FakeDm {
    trace: Rc<RefCell<Vec<&'static str>>>,
    send: Result<u64, DmPortError>,
    edited_content: Option<String>,
}

impl Default for FakeDm {
    fn default() -> Self {
        Self {
            trace: Rc::new(RefCell::new(Vec::new())),
            send: Ok(1),
            edited_content: None,
        }
    }
}

impl LobbyDmPort for FakeDm {
    type Message = u64;

    fn send_preflight(&mut self, _target_name: &str) -> Result<Self::Message, DmPortError> {
        self.trace.borrow_mut().push("dm_check");
        self.send
    }

    fn edit_message(&mut self, _message: &Self::Message, content: &str) -> Result<(), DmPortError> {
        self.trace.borrow_mut().push("dm_edit");
        self.edited_content = Some(content.to_owned());
        Ok(())
    }
}

fn request() -> LobbyAutoNotifyRequest {
    LobbyAutoNotifyRequest {
        subscriber_id: 123,
        guild_id: Some(GUILD),
        enabled: None,
        target: None,
    }
}

fn target_request() -> LobbyAutoNotifyRequest {
    LobbyAutoNotifyRequest {
        target: Some(LobbyTarget {
            discord_id: 456,
            display_name: "Target Player".to_owned(),
            is_bot: false,
        }),
        ..request()
    }
}

#[test]
fn test_no_option_enables_persistent_notifications() {
    let mut reminder = FakeReminder::default();
    let mut dm = FakeDm::default();
    let outcome = execute_lobby_autonotify(request(), Some(&mut reminder), &mut dm);
    assert_eq!(reminder.preference, Some((123, GUILD, true)));
    assert!(outcome.ephemeral && outcome.preference_persisted);
    assert!(outcome.content.contains("now **ON**"));
}

#[test]
fn test_false_disables_persistent_notifications() {
    let mut reminder = FakeReminder::default();
    let mut dm = FakeDm::default();
    let mut input = request();
    input.subscriber_id = 456;
    input.guild_id = Some(789);
    input.enabled = Some(false);
    let outcome = execute_lobby_autonotify(input, Some(&mut reminder), &mut dm);
    assert_eq!(reminder.preference, Some((456, 789, false)));
    assert!(outcome.content.contains("now **OFF**"));
}

#[test]
fn test_missing_reminder_service_returns_ephemeral_error() {
    let mut dm = FakeDm::default();
    let outcome = execute_lobby_autonotify::<FakeReminder, _>(request(), None, &mut dm);
    assert!(outcome.ephemeral);
    assert!(outcome.content.contains("unavailable"));
}

#[test]
fn test_dm_invocation_is_rejected_before_persistence() {
    let mut reminder = FakeReminder::default();
    let mut dm = FakeDm::default();
    let mut input = request();
    input.guild_id = None;
    let outcome = execute_lobby_autonotify(input, Some(&mut reminder), &mut dm);
    assert_eq!(
        outcome.content,
        "This command can only be used in a server."
    );
    assert!(reminder.trace.borrow().is_empty());
}

#[test]
fn test_player_target_checks_dm_before_persisting_subscription() {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let mut reminder = FakeReminder {
        trace: Rc::clone(&trace),
        ..FakeReminder::default()
    };
    let mut dm = FakeDm {
        trace: Rc::clone(&trace),
        ..FakeDm::default()
    };
    let outcome = execute_lobby_autonotify(target_request(), Some(&mut reminder), &mut dm);
    assert_eq!(
        &*trace.borrow(),
        &["dup_check", "dm_check", "persist", "dm_edit"]
    );
    assert!(outcome.content.contains("One-time lobby alert set"));
    assert!(outcome.subscription_persisted && outcome.preflight_dm_edited);
    assert!(
        dm.edited_content
            .expect("edited DM")
            .contains("One-time lobby alert set")
    );
}

#[test]
fn test_player_target_duplicate_updates_dm_to_reflect_no_new_alert() {
    let mut reminder = FakeReminder {
        create: Ok(false),
        ..FakeReminder::default()
    };
    let mut dm = FakeDm::default();
    let outcome = execute_lobby_autonotify(target_request(), Some(&mut reminder), &mut dm);
    assert!(
        outcome
            .content
            .contains("already have a one-time lobby alert")
    );
    assert!(
        dm.edited_content
            .expect("duplicate DM edit")
            .contains("already have a one-time lobby alert")
    );
}

#[test]
fn test_player_target_known_duplicate_skips_preflight_dm() {
    let mut reminder = FakeReminder {
        duplicate: Ok(true),
        ..FakeReminder::default()
    };
    let mut dm = FakeDm::default();
    let outcome = execute_lobby_autonotify(target_request(), Some(&mut reminder), &mut dm);
    assert!(
        outcome
            .content
            .contains("already have a one-time lobby alert")
    );
    assert_eq!(&*reminder.trace.borrow(), &["dup_check"]);
    assert!(dm.trace.borrow().is_empty());
}

#[test]
fn test_player_target_precheck_failure_falls_back_to_preflight() {
    let mut reminder = FakeReminder {
        duplicate: Err(ReminderPortError::Unavailable),
        ..FakeReminder::default()
    };
    let mut dm = FakeDm::default();
    let outcome = execute_lobby_autonotify(target_request(), Some(&mut reminder), &mut dm);
    assert!(outcome.content.contains("One-time lobby alert set"));
    assert!(outcome.preflight_dm_sent && outcome.subscription_persisted);
}

#[test]
fn test_player_target_save_failure_updates_dm_to_reflect_failure() {
    let mut reminder = FakeReminder {
        create: Err(ReminderPortError::Unavailable),
        ..FakeReminder::default()
    };
    let mut dm = FakeDm::default();
    let outcome = execute_lobby_autonotify(target_request(), Some(&mut reminder), &mut dm);
    assert!(outcome.content.contains("couldn't save the alert"));
    assert!(
        dm.edited_content
            .expect("failure DM edit")
            .contains("couldn't save the alert")
    );
}

#[test]
fn test_player_target_blocked_dm_persists_nothing() {
    let mut reminder = FakeReminder::default();
    let mut dm = FakeDm {
        send: Err(DmPortError::Forbidden),
        ..FakeDm::default()
    };
    let outcome = execute_lobby_autonotify(target_request(), Some(&mut reminder), &mut dm);
    assert!(outcome.content.contains("can't DM you"));
    assert_eq!(&*reminder.trace.borrow(), &["dup_check"]);
    assert!(!outcome.subscription_persisted && !outcome.preference_persisted);
}

#[test]
fn test_enter_mmr_button_callback_has_correct_signature() {
    let mut view = MmrPromptView { attempts_left: 3 };
    let mut interaction = MmrInteraction::default();
    let mut button = MmrButton::default();
    view.enter_mmr(&mut interaction, &mut button);
    assert!(interaction.modal_presented);
    assert!(!button.disabled);
}

#[test]
fn test_steam_id_at_uint32_ceiling_rejected() {
    assert_eq!(
        validate_steam_id(STEAM_ACCOUNT_ID_UPPER_BOUND),
        Err(SteamIdValidationError::OutOfRange)
    );
}

#[test]
fn test_register_with_invalid_steam_id() {
    for invalid in [-1, 0, STEAM_ACCOUNT_ID_UPPER_BOUND] {
        assert_eq!(
            validate_steam_id(invalid),
            Err(SteamIdValidationError::OutOfRange)
        );
    }
    assert_eq!(validate_steam_id(123_456_789), Ok(()));
    assert_eq!(validate_steam_id(2_147_483_648), Ok(()));
}

#[derive(Default)]
struct FakeRolePort {
    players: RefCell<BTreeMap<(i64, i64), Vec<String>>>,
}

impl FakeRolePort {
    fn player(&self, discord_id: i64, roles: &[&str]) {
        self.players.borrow_mut().insert(
            (discord_id, GUILD),
            roles.iter().map(|role| (*role).to_owned()).collect(),
        );
    }
}

impl RegistrationRolePort for FakeRolePort {
    type Error = Infallible;

    fn set_roles(
        &self,
        discord_id: i64,
        guild_id: i64,
        roles: &[String],
    ) -> Result<bool, Self::Error> {
        let mut players = self.players.borrow_mut();
        let Some(stored) = players.get_mut(&(discord_id, guild_id)) else {
            return Ok(false);
        };
        *stored = roles.to_vec();
        Ok(true)
    }
}

#[test]
fn test_duplicate_roles_deduplicated_and_persisted() {
    let port = FakeRolePort::default();
    port.player(54_321, &[]);
    let outcome = execute_set_roles(&port, 54_321, GUILD, "1111111111");
    assert_eq!(outcome.persisted_roles, Some(vec!["1".to_owned()]));
    assert!(outcome.content.contains("Set your preferred roles"));
}

#[test]
fn test_mixed_duplicates_preserve_order() {
    let port = FakeRolePort::default();
    port.player(54_322, &[]);
    let outcome = execute_set_roles(&port, 54_322, GUILD, "54321123");
    assert_eq!(
        outcome.persisted_roles,
        Some(["5", "4", "3", "2", "1"].map(str::to_owned).to_vec())
    );
}

#[test]
fn test_comma_separated_with_duplicates() {
    let port = FakeRolePort::default();
    port.player(54_323, &[]);
    let outcome = execute_set_roles(&port, 54_323, GUILD, "1, 2, 1, 3, 2");
    assert_eq!(
        outcome.persisted_roles,
        Some(["1", "2", "3"].map(str::to_owned).to_vec())
    );
}

#[test]
fn test_invalid_role_rejected_without_persisting() {
    let port = FakeRolePort::default();
    port.player(54_324, &["2"]);
    let outcome = execute_set_roles(&port, 54_324, GUILD, "126");
    assert!(outcome.content.contains("Invalid role: 6"));
    assert_eq!(
        port.players.borrow().get(&(54_324, GUILD)),
        Some(&vec!["2".to_owned()])
    );
}

#[test]
fn test_empty_roles_rejected() {
    let port = FakeRolePort::default();
    port.player(54_325, &[]);
    let outcome = execute_set_roles(&port, 54_325, GUILD, ", ");
    assert!(outcome.content.contains("at least one role"));
    assert!(outcome.persisted_roles.is_none());
}

#[test]
fn test_unregistered_player_gets_error() {
    let port = FakeRolePort::default();
    let outcome = execute_set_roles(&port, 98_765, GUILD, "123");
    assert!(outcome.content.to_lowercase().contains("not registered"));
}
