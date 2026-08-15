use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use super::*;

const NOW: i64 = 1_000_000;
const TEST_GUILD_ID: i64 = 12_345;
const TEST_GUILD_ID_2: i64 = 99_999;

fn user(value: i64) -> UserId {
    UserId::new(value)
}

fn guild(value: i64) -> GuildId {
    GuildId::new(value)
}

fn task_key(user_id: i64, guild_id: i64, kind: ReminderKind) -> ReminderTaskKey {
    ReminderTaskKey::new(user(user_id), guild(guild_id), kind)
}

#[derive(Clone)]
struct FixedClock {
    seconds: Arc<AtomicI64>,
    nanoseconds: Arc<AtomicI64>,
}

impl FixedClock {
    fn new(seconds: i64) -> Self {
        Self {
            seconds: Arc::new(AtomicI64::new(seconds)),
            nanoseconds: Arc::new(AtomicI64::new(seconds.saturating_mul(1_000_000_000))),
        }
    }

    fn set_nanoseconds(&self, nanoseconds: i64) {
        self.nanoseconds.store(nanoseconds, Ordering::SeqCst);
    }
}

impl ReminderClock for FixedClock {
    fn now_seconds(&self) -> i64 {
        self.seconds.load(Ordering::SeqCst)
    }

    fn now_nanoseconds(&self) -> i64 {
        self.nanoseconds.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct RecordingDiscord {
    attempts: Vec<(UserId, String)>,
    failures: BTreeSet<UserId>,
    completed_attempts: usize,
}

impl RecordingDiscord {
    fn fail_for(&mut self, user_id: UserId) {
        self.failures.insert(user_id);
    }
}

impl DiscordPort for RecordingDiscord {
    fn send_dm(&mut self, user_id: UserId, message: &str) -> Result<(), PortError> {
        self.attempts.push((user_id, message.to_owned()));
        self.completed_attempts += 1;
        if self.failures.contains(&user_id) {
            Err(PortError::new("send_dm", "Forbidden"))
        } else {
            Ok(())
        }
    }
}

type TestService = ReminderService<InMemoryReminderStore, FixedClock, RecordingDiscord>;

fn fixture() -> TestService {
    ReminderService::new(
        InMemoryReminderStore::default(),
        FixedClock::new(NOW),
        RecordingDiscord::default(),
    )
}

fn set_store_preference(
    store: &InMemoryReminderStore,
    user_id: i64,
    guild_id: i64,
    kind: ReminderKind,
    enabled: bool,
) {
    store
        .set_preference(user(user_id), guild(guild_id), kind, enabled)
        .unwrap();
}

#[test]
fn test_defaults_when_no_row() {
    let store = InMemoryReminderStore::default();
    let preferences = store
        .get_preferences(user(9_001), guild(TEST_GUILD_ID))
        .unwrap();
    assert!(
        ReminderKind::ALL
            .into_iter()
            .all(|kind| !preferences.enabled(kind))
    );
}

#[test]
fn test_set_wheel_preference() {
    let store = InMemoryReminderStore::default();
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Wheel, true);
    let preferences = store
        .get_preferences(user(1), guild(TEST_GUILD_ID))
        .unwrap();
    assert!(preferences.enabled(ReminderKind::Wheel));
    assert!(!preferences.enabled(ReminderKind::Trivia));
    assert!(!preferences.enabled(ReminderKind::Betting));
}

#[test]
fn test_set_preference_idempotent() {
    let store = InMemoryReminderStore::default();
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Betting, true);
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Betting, true);
    assert!(
        store
            .get_preferences(user(1), guild(TEST_GUILD_ID))
            .unwrap()
            .enabled(ReminderKind::Betting)
    );
}

#[test]
fn test_set_preference_toggle_off() {
    let store = InMemoryReminderStore::default();
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Trivia, true);
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Trivia, false);
    assert!(
        !store
            .get_preferences(user(1), guild(TEST_GUILD_ID))
            .unwrap()
            .enabled(ReminderKind::Trivia)
    );
}

#[test]
fn test_set_lobby_preference_and_load_subscribers() {
    let store = InMemoryReminderStore::default();
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Lobby, true);
    set_store_preference(&store, 2, TEST_GUILD_ID, ReminderKind::Lobby, false);
    set_store_preference(&store, 3, TEST_GUILD_ID_2, ReminderKind::Lobby, true);
    assert!(
        store
            .get_preferences(user(1), guild(TEST_GUILD_ID))
            .unwrap()
            .enabled(ReminderKind::Lobby)
    );
    assert_eq!(
        store
            .get_enabled_users_for_type(guild(TEST_GUILD_ID), ReminderKind::Lobby)
            .unwrap(),
        [user(1)]
    );
}

#[test]
fn test_get_enabled_users_for_type() {
    let store = InMemoryReminderStore::default();
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Wheel, true);
    set_store_preference(&store, 2, TEST_GUILD_ID, ReminderKind::Wheel, false);
    set_store_preference(&store, 3, TEST_GUILD_ID, ReminderKind::Wheel, true);
    assert_eq!(
        store
            .get_enabled_users_for_type(guild(TEST_GUILD_ID), ReminderKind::Wheel)
            .unwrap(),
        [user(1), user(3)]
    );
}

#[test]
fn test_get_enabled_users_empty() {
    let store = InMemoryReminderStore::default();
    assert!(
        store
            .get_enabled_users_for_type(guild(TEST_GUILD_ID), ReminderKind::Betting)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_get_enabled_users_by_type_bulk() {
    let store = InMemoryReminderStore::default();
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Wheel, true);
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Dig, true);
    set_store_preference(&store, 2, TEST_GUILD_ID, ReminderKind::Trivia, true);
    set_store_preference(&store, 3, TEST_GUILD_ID + 1, ReminderKind::Wheel, true);
    let kinds = valid_reminder_kinds(["wheel", "trivia", "dig", "invalid", "wheel"]);
    assert_eq!(
        kinds,
        [ReminderKind::Wheel, ReminderKind::Trivia, ReminderKind::Dig]
    );
    assert_eq!(
        store
            .get_enabled_users_by_type_bulk(guild(TEST_GUILD_ID), &kinds)
            .unwrap(),
        BTreeMap::from([
            (ReminderKind::Wheel, vec![user(1)]),
            (ReminderKind::Trivia, vec![user(2)]),
            (ReminderKind::Dig, vec![user(1)]),
        ])
    );
}

#[test]
fn test_invalid_type_raises() {
    let error = ReminderKind::from_str("invalid").unwrap_err();
    assert_eq!(error.value(), "invalid");
    assert_eq!(error.to_string(), "invalid reminder type: \"invalid\"");
}

#[test]
fn test_guild_isolation() {
    let store = InMemoryReminderStore::default();
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Wheel, true);
    set_store_preference(&store, 1, TEST_GUILD_ID_2, ReminderKind::Wheel, false);
    assert!(
        store
            .get_preferences(user(1), guild(TEST_GUILD_ID))
            .unwrap()
            .enabled(ReminderKind::Wheel)
    );
    assert!(
        !store
            .get_preferences(user(1), guild(TEST_GUILD_ID_2))
            .unwrap()
            .enabled(ReminderKind::Wheel)
    );
}

#[test]
fn test_guild_id_none_normalized() {
    let mut service = fixture();
    service
        .set_preference(user(1), None, ReminderKind::Trivia, true)
        .unwrap();
    assert!(
        service
            .get_preferences(user(1), None)
            .unwrap()
            .enabled(ReminderKind::Trivia)
    );
    assert!(
        service
            .get_preferences(user(1), Some(GuildId::DIRECT_MESSAGE))
            .unwrap()
            .enabled(ReminderKind::Trivia)
    );
}

#[test]
fn test_lobby_target_subscription_is_idempotent_and_claimed_once() {
    let store = InMemoryReminderStore::default();
    let guild_id = guild(TEST_GUILD_ID);
    assert!(
        store
            .add_lobby_target_subscription(user(1), user(10), guild_id, 100)
            .unwrap()
    );
    assert!(
        !store
            .add_lobby_target_subscription(user(1), user(10), guild_id, 200)
            .unwrap()
    );
    assert_eq!(
        store
            .claim_lobby_target_subscribers(user(10), guild_id, i64::MAX)
            .unwrap(),
        [user(1)]
    );
    assert!(
        store
            .claim_lobby_target_subscribers(user(10), guild_id, i64::MAX)
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .get_preferences(user(1), guild_id)
            .unwrap()
            .enabled(ReminderKind::Lobby)
    );
    assert!(
        store
            .get_enabled_users_for_type(guild_id, ReminderKind::Lobby)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_lobby_target_subscriptions_support_many_targets_and_subscribers() {
    let store = InMemoryReminderStore::default();
    let guild_id = guild(TEST_GUILD_ID);
    assert!(
        store
            .add_lobby_target_subscription(user(1), user(10), guild_id, 100)
            .unwrap()
    );
    assert!(
        store
            .add_lobby_target_subscription(user(1), user(11), guild_id, 110)
            .unwrap()
    );
    assert!(
        store
            .add_lobby_target_subscription(user(2), user(10), guild_id, 120)
            .unwrap()
    );
    assert_eq!(
        store
            .claim_lobby_target_subscribers(user(10), guild_id, i64::MAX)
            .unwrap(),
        [user(1), user(2)]
    );
    assert_eq!(
        store
            .claim_lobby_target_subscribers(user(11), guild_id, i64::MAX)
            .unwrap(),
        [user(1)]
    );
}

#[test]
fn test_lobby_target_subscription_claim_is_guild_scoped() {
    let store = InMemoryReminderStore::default();
    store
        .add_lobby_target_subscription(user(1), user(10), guild(TEST_GUILD_ID), 100)
        .unwrap();
    store
        .add_lobby_target_subscription(user(1), user(10), guild(TEST_GUILD_ID_2), 100)
        .unwrap();
    assert_eq!(
        store
            .claim_lobby_target_subscribers(user(10), guild(TEST_GUILD_ID), i64::MAX)
            .unwrap(),
        [user(1)]
    );
    assert!(
        store
            .claim_lobby_target_subscribers(user(10), guild(TEST_GUILD_ID), i64::MAX)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .claim_lobby_target_subscribers(user(10), guild(TEST_GUILD_ID_2), i64::MAX)
            .unwrap(),
        [user(1)]
    );
}

#[test]
fn test_has_lobby_target_subscription_reflects_add_and_claim() {
    let store = InMemoryReminderStore::default();
    let guild_id = guild(TEST_GUILD_ID);
    assert!(
        !store
            .has_lobby_target_subscription(user(1), user(10), guild_id)
            .unwrap()
    );
    store
        .add_lobby_target_subscription(user(1), user(10), guild_id, 100)
        .unwrap();
    assert!(
        store
            .has_lobby_target_subscription(user(1), user(10), guild_id)
            .unwrap()
    );
    assert!(
        !store
            .has_lobby_target_subscription(user(2), user(10), guild_id)
            .unwrap()
    );
    assert!(
        !store
            .has_lobby_target_subscription(user(1), user(11), guild_id)
            .unwrap()
    );
    assert!(
        !store
            .has_lobby_target_subscription(user(1), user(10), guild(TEST_GUILD_ID_2))
            .unwrap()
    );
    store
        .claim_lobby_target_subscribers(user(10), guild_id, i64::MAX)
        .unwrap();
    assert!(
        !store
            .has_lobby_target_subscription(user(1), user(10), guild_id)
            .unwrap()
    );
}

#[test]
fn test_service_pass_through_reads_target_subscription() {
    let service = fixture();
    service.clock().set_nanoseconds(100);
    assert!(
        !service
            .has_lobby_player_subscription(user(1), user(10), Some(guild(TEST_GUILD_ID)))
            .unwrap()
    );
    assert!(
        service
            .add_lobby_player_subscription(user(1), user(10), Some(guild(TEST_GUILD_ID)))
            .unwrap()
    );
    assert!(
        service
            .has_lobby_player_subscription(user(1), user(10), Some(guild(TEST_GUILD_ID)))
            .unwrap()
    );
}

#[test]
fn test_join_cutoff_leaves_subscriptions_created_after_the_join() {
    let store = InMemoryReminderStore::default();
    let guild_id = guild(TEST_GUILD_ID);
    store
        .add_lobby_target_subscription(user(1), user(10), guild_id, 100)
        .unwrap();
    store
        .add_lobby_target_subscription(user(2), user(10), guild_id, 200)
        .unwrap();
    assert_eq!(
        store
            .claim_lobby_target_subscribers(user(10), guild_id, 150)
            .unwrap(),
        [user(1)]
    );
    assert_eq!(
        store
            .claim_lobby_target_subscribers(user(10), guild_id, 300)
            .unwrap(),
        [user(2)]
    );
}

#[test]
fn test_toggle_off_to_on() {
    let mut service = fixture();
    assert!(
        service
            .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel)
            .unwrap()
    );
}

#[test]
fn test_toggle_on_to_off() {
    let mut service = fixture();
    service
        .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel)
        .unwrap();
    assert!(
        !service
            .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel)
            .unwrap()
    );
}

#[test]
fn test_get_preferences_proxy() {
    let mut service = fixture();
    service
        .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Betting)
        .unwrap();
    assert!(
        service
            .get_preferences(user(1), Some(guild(TEST_GUILD_ID)))
            .unwrap()
            .enabled(ReminderKind::Betting)
    );
}

#[test]
fn test_set_and_load_lobby_preference() {
    let mut service = fixture();
    assert!(
        service
            .set_preference(
                user(1),
                Some(guild(TEST_GUILD_ID)),
                ReminderKind::Lobby,
                true,
            )
            .unwrap()
    );
    assert_eq!(
        service
            .get_lobby_subscriber_ids(Some(guild(TEST_GUILD_ID)), None)
            .unwrap(),
        [user(1)]
    );
    assert!(
        !service
            .set_preference(
                user(1),
                Some(guild(TEST_GUILD_ID)),
                ReminderKind::Lobby,
                false,
            )
            .unwrap()
    );
    assert!(
        service
            .get_lobby_subscriber_ids(Some(guild(TEST_GUILD_ID)), None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_lowskill_lobby_subscribers_require_rating_below_1400() {
    let mut service = fixture();
    for user_id in 1..=4 {
        service
            .set_preference(
                user(user_id),
                Some(guild(TEST_GUILD_ID)),
                ReminderKind::Lobby,
                true,
            )
            .unwrap();
    }
    for (user_id, rating) in [(1, 1_399.0), (2, 1_400.0), (3, 1_401.0)] {
        service
            .store()
            .seed_player_rating(user(user_id), guild(TEST_GUILD_ID), Some(rating))
            .unwrap();
    }
    assert_eq!(
        service
            .get_lobby_subscriber_ids(Some(guild(TEST_GUILD_ID)), Some(LobbyKind::LowSkill))
            .unwrap(),
        [user(1)]
    );
}

#[test]
fn test_successful_delivery_consumes_subscription_once() {
    let mut service = fixture();
    service.clock().set_nanoseconds(100);
    service
        .add_lobby_player_subscription(user(1), user(10), Some(guild(TEST_GUILD_ID)))
        .unwrap();
    let attempted = service
        .notify_lobby_player_subscribers(
            user(10),
            "Target Player",
            Some(guild(TEST_GUILD_ID)),
            Some(LobbyKind::LowSkill),
            None,
        )
        .unwrap();
    let repeated = service
        .notify_lobby_player_subscribers(
            user(10),
            "Target Player",
            Some(guild(TEST_GUILD_ID)),
            Some(LobbyKind::LowSkill),
            None,
        )
        .unwrap();
    assert_eq!(attempted.attempted_count(), 1);
    assert_eq!(repeated.attempted_count(), 0);
    assert_eq!(service.discord().attempts.len(), 1);
    let message = &service.discord().attempts[0].1;
    assert!(message.contains("Target Player"));
    assert!(message.contains(LobbyKind::LowSkill.label()));
    assert!(message.contains("one-time"));
}

#[test]
fn test_target_display_name_is_escaped_in_runtime_dm() {
    let mut service = fixture();
    service.clock().set_nanoseconds(100);
    service
        .add_lobby_player_subscription(user(1), user(10), Some(guild(TEST_GUILD_ID)))
        .unwrap();
    let crafted_name = "**<@123> @everyone [click](https://example.invalid)**";
    service
        .notify_lobby_player_subscribers(
            user(10),
            crafted_name,
            Some(guild(TEST_GUILD_ID)),
            None,
            None,
        )
        .unwrap();
    let message = &service.discord().attempts[0].1;
    assert!(!message.contains(crafted_name));
    assert!(!message.contains("<@123>"));
    assert!(!message.contains("@everyone"));
    assert!(message.contains("<@\u{200b}123\\>"));
    assert!(message.contains("@\u{200b}everyone"));
    assert!(message.contains("\\*\\*"));
    assert!(message.contains("\\[click\\]\\("));
}

#[test]
fn test_failed_delivery_still_consumes_subscription() {
    let mut discord = RecordingDiscord::default();
    discord.fail_for(user(1));
    let clock = FixedClock::new(NOW);
    clock.set_nanoseconds(100);
    let mut service = ReminderService::new(InMemoryReminderStore::default(), clock, discord);
    service
        .add_lobby_player_subscription(user(1), user(10), Some(guild(TEST_GUILD_ID)))
        .unwrap();
    let attempted = service
        .notify_lobby_player_subscribers(
            user(10),
            "Target Player",
            Some(guild(TEST_GUILD_ID)),
            None,
            None,
        )
        .unwrap();
    let repeated = service
        .notify_lobby_player_subscribers(
            user(10),
            "Target Player",
            Some(guild(TEST_GUILD_ID)),
            None,
            None,
        )
        .unwrap();
    assert_eq!(attempted.attempted_count(), 1);
    assert_eq!(attempted.failed.len(), 1);
    assert_eq!(repeated.attempted_count(), 0);
    assert_eq!(service.discord().attempts.len(), 1);
}

#[test]
fn test_no_task_when_pref_disabled() {
    let mut service = fixture();
    let scheduled = service
        .schedule_wheel_reminder(user(1), Some(guild(TEST_GUILD_ID)), NOW + 3_600, None)
        .unwrap();
    assert_eq!(scheduled, None);
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Wheel))
            .is_none()
    );
}

#[test]
fn test_task_created_when_pref_enabled() {
    let mut service = fixture();
    service
        .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel)
        .unwrap();
    let task_id = service
        .schedule_wheel_reminder(user(1), Some(guild(TEST_GUILD_ID)), NOW + 3_600, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Wheel))
            .unwrap()
            .id,
        task_id
    );
}

#[test]
fn test_rescheduling_cancels_old_task() {
    let mut service = fixture();
    service
        .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Trivia)
        .unwrap();
    let first = service
        .schedule_trivia_reminder(user(1), Some(guild(TEST_GUILD_ID)), NOW + 3_600, None)
        .unwrap()
        .unwrap();
    let second = service
        .schedule_trivia_reminder(user(1), Some(guild(TEST_GUILD_ID)), NOW + 3_700, None)
        .unwrap()
        .unwrap();
    assert_ne!(first, second);
    assert!(service.was_cancelled(first));
    assert_eq!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Trivia))
            .unwrap()
            .id,
        second
    );
}

#[test]
fn test_finished_task_removed_from_tasks() {
    let mut service = fixture();
    service
        .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel)
        .unwrap();
    let task_id = service
        .schedule_wheel_reminder(user(1), Some(guild(TEST_GUILD_ID)), NOW - 10, None)
        .unwrap()
        .unwrap();
    let dispatched = service.dispatch_due();
    assert_eq!(dispatched[0].0, task_id);
    assert_eq!(dispatched[0].1.delivered, [user(1)]);
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Wheel))
            .is_none()
    );
}

#[test]
fn test_disabling_cancels_task() {
    let mut service = fixture();
    service
        .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel)
        .unwrap();
    let task_id = service
        .schedule_wheel_reminder(user(1), Some(guild(TEST_GUILD_ID)), NOW + 3_600, None)
        .unwrap()
        .unwrap();
    assert!(
        !service
            .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel)
            .unwrap()
    );
    assert!(service.was_cancelled(task_id));
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Wheel))
            .is_none()
    );
}

#[test]
fn test_reschedule_skips_expired_cooldown() {
    let mut service = fixture();
    set_store_preference(service.store(), 1, TEST_GUILD_ID, ReminderKind::Wheel, true);
    service
        .store()
        .seed_reminder_timestamps(
            user(1),
            guild(TEST_GUILD_ID),
            ReminderTimestamps {
                last_wheel_spin: Some(NOW - 200_000),
                ..ReminderTimestamps::default()
            },
        )
        .unwrap();
    service.reschedule_all(&[Some(guild(TEST_GUILD_ID))]);
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Wheel))
            .is_none()
    );
}

#[test]
fn test_reschedule_creates_task_for_active_cooldown() {
    let mut service = fixture();
    set_store_preference(service.store(), 1, TEST_GUILD_ID, ReminderKind::Wheel, true);
    service
        .store()
        .seed_reminder_timestamps(
            user(1),
            guild(TEST_GUILD_ID),
            ReminderTimestamps {
                last_wheel_spin: Some(NOW - 100),
                ..ReminderTimestamps::default()
            },
        )
        .unwrap();
    service.reschedule_all(&[Some(guild(TEST_GUILD_ID))]);
    assert_eq!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Wheel))
            .unwrap()
            .due_at,
        NOW - 100 + WHEEL_COOLDOWN_SECONDS
    );
}

#[test]
fn test_reschedule_skips_none_last_spin() {
    let mut service = fixture();
    set_store_preference(service.store(), 1, TEST_GUILD_ID, ReminderKind::Wheel, true);
    service
        .store()
        .seed_reminder_timestamps(user(1), guild(TEST_GUILD_ID), ReminderTimestamps::default())
        .unwrap();
    service.reschedule_all(&[Some(guild(TEST_GUILD_ID))]);
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Wheel))
            .is_none()
    );
}

#[test]
fn test_notify_betting_no_subscribers() {
    let mut service = fixture();
    let report = service
        .notify_betting_subscribers(Some(guild(TEST_GUILD_ID)), NOW + 900, None, None)
        .unwrap();
    assert_eq!(report, DeliveryReport::default());
    assert!(service.discord().attempts.is_empty());
}

#[test]
fn test_notify_betting_dms_subscribers() {
    let mut service = fixture();
    for user_id in [1, 2] {
        service
            .toggle_preference(
                user(user_id),
                Some(guild(TEST_GUILD_ID)),
                ReminderKind::Betting,
            )
            .unwrap();
    }
    let report = service
        .notify_betting_subscribers(Some(guild(TEST_GUILD_ID)), NOW + 900, None, None)
        .unwrap();
    assert_eq!(report.delivered, [user(1), user(2)]);
    assert_eq!(
        service
            .discord()
            .attempts
            .iter()
            .map(|(user_id, _)| *user_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([user(1), user(2)])
    );
}

#[test]
fn test_notify_betting_identifies_lobby_and_pending_match() {
    let mut service = fixture();
    service
        .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Betting)
        .unwrap();
    service
        .notify_betting_subscribers(
            Some(guild(TEST_GUILD_ID)),
            NOW + 900,
            Some(42),
            Some(LobbyKind::LowSkill),
        )
        .unwrap();
    assert_eq!(
        service.discord().attempts[0].1,
        "A new 🧀 Whine & Cheese match (Match #42) has been shuffled! Betting is open for ~15 more minutes. Use `/bet` now!"
    );
}

#[test]
fn test_notify_betting_awaits_dms_before_returning() {
    let mut service = fixture();
    for user_id in [1, 2] {
        service
            .toggle_preference(
                user(user_id),
                Some(guild(TEST_GUILD_ID)),
                ReminderKind::Betting,
            )
            .unwrap();
    }
    let report = service
        .notify_betting_subscribers(Some(guild(TEST_GUILD_ID)), NOW + 900, None, None)
        .unwrap();
    assert_eq!(report.attempted_count(), 2);
    assert_eq!(service.discord().completed_attempts, 2);
}

#[test]
fn test_dig_preference_toggle() {
    let store = InMemoryReminderStore::default();
    set_store_preference(&store, 1, TEST_GUILD_ID, ReminderKind::Dig, true);
    assert!(
        store
            .get_preferences(user(1), guild(TEST_GUILD_ID))
            .unwrap()
            .enabled(ReminderKind::Dig)
    );
}

#[test]
fn test_dig_schedule_creates_task_when_enabled() {
    let mut service = fixture();
    service
        .toggle_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Dig)
        .unwrap();
    let task_id = service
        .schedule_dig_reminder(user(1), Some(guild(TEST_GUILD_ID)), NOW + 3_600, None)
        .unwrap()
        .unwrap();
    let task = service
        .task(task_key(1, TEST_GUILD_ID, ReminderKind::Dig))
        .unwrap();
    assert_eq!(task.id, task_id);
    assert_eq!(task.message, DIG_REMINDER_MESSAGE);
    assert_eq!(task.delay_seconds, 3_600);
}

#[test]
fn test_dig_no_task_when_disabled() {
    let mut service = fixture();
    assert_eq!(
        service
            .schedule_dig_reminder(user(1), Some(guild(TEST_GUILD_ID)), NOW + 3_600, None,)
            .unwrap(),
        None
    );
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Dig))
            .is_none()
    );
}

#[test]
fn test_reschedule_dig_uses_authoritative_ready_at() {
    let mut service = fixture();
    let ready_at = NOW + 5_400;
    set_store_preference(service.store(), 1, TEST_GUILD_ID, ReminderKind::Dig, true);
    service
        .store()
        .seed_dig_ready_at(user(1), guild(TEST_GUILD_ID), Some(ready_at))
        .unwrap();
    service.reschedule_all(&[Some(guild(TEST_GUILD_ID))]);
    let task = service
        .task(task_key(1, TEST_GUILD_ID, ReminderKind::Dig))
        .unwrap();
    assert_eq!(task.due_at, ready_at);
    assert_eq!(task.delay_seconds, ready_at - NOW);
}

#[test]
fn test_reschedule_dig_skips_already_ready() {
    let mut service = fixture();
    set_store_preference(service.store(), 1, TEST_GUILD_ID, ReminderKind::Dig, true);
    service
        .store()
        .seed_dig_ready_at(user(1), guild(TEST_GUILD_ID), None)
        .unwrap();
    service.reschedule_all(&[Some(guild(TEST_GUILD_ID))]);
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Dig))
            .is_none()
    );
}

#[test]
fn test_reconcile_schedules_exact_authoritative_timestamp() {
    let mut service = fixture();
    let ready_at = NOW + 9_000;
    set_store_preference(service.store(), 1, TEST_GUILD_ID, ReminderKind::Dig, true);
    service
        .store()
        .seed_dig_ready_at(user(1), guild(TEST_GUILD_ID), Some(ready_at))
        .unwrap();
    let outcome = service
        .reconcile_dig_reminder(user(1), Some(guild(TEST_GUILD_ID)), Some(NOW), None)
        .unwrap();
    assert!(matches!(outcome, DigReconcileOutcome::Scheduled(_)));
    let task = service
        .task(task_key(1, TEST_GUILD_ID, ReminderKind::Dig))
        .unwrap();
    assert_eq!(task.due_at, ready_at);
    assert_eq!(task.delay_seconds, 9_000);
    assert_eq!(task.message, DIG_REMINDER_MESSAGE);
}

fn assert_reconcile_cancels_stale_task(ready_at: Option<i64>) {
    let mut service = fixture();
    service
        .set_preference(user(1), Some(guild(TEST_GUILD_ID)), ReminderKind::Dig, true)
        .unwrap();
    let stale_task = service
        .schedule_dig_reminder(user(1), Some(guild(TEST_GUILD_ID)), NOW + 100, None)
        .unwrap()
        .unwrap();
    service
        .store()
        .seed_dig_ready_at(user(1), guild(TEST_GUILD_ID), ready_at)
        .unwrap();
    let outcome = service
        .reconcile_dig_reminder(user(1), Some(guild(TEST_GUILD_ID)), Some(NOW), None)
        .unwrap();
    assert_eq!(outcome, DigReconcileOutcome::Cancelled(Some(stale_task)));
    assert!(service.was_cancelled(stale_task));
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Dig))
            .is_none()
    );
}

#[test]
fn test_reconcile_cancels_stale_task_when_already_ready_none() {
    assert_reconcile_cancels_stale_task(None);
}

#[test]
fn test_reconcile_cancels_stale_task_when_already_ready_zero() {
    assert_reconcile_cancels_stale_task(Some(NOW));
}

#[test]
fn test_reconcile_cancels_stale_task_when_already_ready_negative_one() {
    assert_reconcile_cancels_stale_task(Some(NOW - 1));
}

#[test]
fn test_repeated_reconcile_replaces_with_one_task() {
    let mut service = fixture();
    set_store_preference(service.store(), 1, TEST_GUILD_ID, ReminderKind::Dig, true);
    service
        .store()
        .seed_dig_ready_at(user(1), guild(TEST_GUILD_ID), Some(NOW + 100))
        .unwrap();
    let first = match service
        .reconcile_dig_reminder(user(1), Some(guild(TEST_GUILD_ID)), Some(NOW), None)
        .unwrap()
    {
        DigReconcileOutcome::Scheduled(task_id) => task_id,
        outcome => panic!("expected schedule, got {outcome:?}"),
    };
    service
        .store()
        .seed_dig_ready_at(user(1), guild(TEST_GUILD_ID), Some(NOW + 200))
        .unwrap();
    let second = match service
        .reconcile_dig_reminder(user(1), Some(guild(TEST_GUILD_ID)), Some(NOW), None)
        .unwrap()
    {
        DigReconcileOutcome::Scheduled(task_id) => task_id,
        outcome => panic!("expected schedule, got {outcome:?}"),
    };
    assert_ne!(first, second);
    assert!(service.was_cancelled(first));
    assert_eq!(service.tasks().len(), 1);
    assert_eq!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Dig))
            .unwrap()
            .due_at,
        NOW + 200
    );
}

#[test]
fn test_reconcile_keeps_same_user_guilds_independent() {
    let mut service = fixture();
    for (guild_id, ready_at) in [(TEST_GUILD_ID, NOW + 100), (TEST_GUILD_ID_2, NOW + 200)] {
        set_store_preference(&service.store, 1, guild_id, ReminderKind::Dig, true);
        service
            .store()
            .seed_dig_ready_at(user(1), guild(guild_id), Some(ready_at))
            .unwrap();
        service
            .reconcile_dig_reminder(user(1), Some(guild(guild_id)), Some(NOW), None)
            .unwrap();
    }
    let guild_one = task_key(1, TEST_GUILD_ID, ReminderKind::Dig);
    let guild_two = task_key(1, TEST_GUILD_ID_2, ReminderKind::Dig);
    let guild_two_task = service.task(guild_two).unwrap().id;
    assert_eq!(
        service.tasks().keys().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([guild_one, guild_two])
    );
    service.cancel_dig_reminder(user(1), Some(guild(TEST_GUILD_ID)));
    assert!(service.task(guild_one).is_none());
    assert_eq!(service.task(guild_two).unwrap().id, guild_two_task);
}

#[test]
fn test_stale_r0_reconcile_cannot_overwrite_live_r1() {
    let mut service = fixture();
    let r0 = NOW;
    let r1 = r0 + 100;
    let old_ready_at = r0 + 7_200;
    let live_ready_at = r1 + 9_000;
    set_store_preference(service.store(), 1, TEST_GUILD_ID, ReminderKind::Dig, true);
    service
        .store()
        .seed_dig_ready_at(user(1), guild(TEST_GUILD_ID), Some(old_ready_at))
        .unwrap();
    let stale_ticket = service
        .begin_dig_reconcile(user(1), Some(guild(TEST_GUILD_ID)), r0)
        .unwrap();
    let stale_read = service
        .read_authoritative_dig_ready_at(stale_ticket)
        .unwrap();
    service
        .store()
        .seed_dig_ready_at(user(1), guild(TEST_GUILD_ID), Some(live_ready_at))
        .unwrap();
    let live_ticket = service
        .begin_dig_reconcile(user(1), Some(guild(TEST_GUILD_ID)), r1)
        .unwrap();
    let live_read = service
        .read_authoritative_dig_ready_at(live_ticket)
        .unwrap();
    assert_eq!(service.dig_reconcile_version(live_ticket.key()), Some(2));
    assert_eq!(
        service
            .finish_dig_reconcile(stale_ticket, stale_read, Some(true))
            .unwrap(),
        DigReconcileOutcome::Superseded
    );
    assert!(matches!(
        service
            .finish_dig_reconcile(live_ticket, live_read, Some(true))
            .unwrap(),
        DigReconcileOutcome::Scheduled(_)
    ));
    let task = service.task(live_ticket.key()).unwrap();
    assert_eq!(task.due_at, live_ready_at);
    assert_ne!(task.due_at, old_ready_at);
    assert_eq!(service.tasks().len(), 1);
}

#[test]
fn test_recovery_completely_reconciles_existing_dig_tasks() {
    let mut service = fixture();
    for user_id in [1, 2] {
        set_store_preference(
            service.store(),
            user_id,
            TEST_GUILD_ID,
            ReminderKind::Dig,
            true,
        );
        service
            .schedule_dig_reminder(
                user(user_id),
                Some(guild(TEST_GUILD_ID)),
                NOW + 100,
                Some(true),
            )
            .unwrap();
    }
    set_store_preference(service.store(), 1, TEST_GUILD_ID, ReminderKind::Dig, false);
    service
        .store()
        .seed_dig_ready_at(user(2), guild(TEST_GUILD_ID), None)
        .unwrap();
    service.reschedule_all(&[Some(guild(TEST_GUILD_ID))]);
    assert!(
        service
            .tasks()
            .keys()
            .all(|key| key.guild_id != guild(TEST_GUILD_ID) || key.kind != ReminderKind::Dig)
    );
}

#[test]
fn test_recovery_continues_when_bulk_dig_result_omits_subscriber() {
    let mut service = fixture();
    for (user_id, guild_id) in [(1, TEST_GUILD_ID), (2, TEST_GUILD_ID), (1, TEST_GUILD_ID_2)] {
        set_store_preference(service.store(), user_id, guild_id, ReminderKind::Dig, true);
    }
    service
        .store()
        .omit_dig_ready_at(user(1), guild(TEST_GUILD_ID))
        .unwrap();
    service
        .store()
        .seed_dig_ready_at(user(2), guild(TEST_GUILD_ID), Some(NOW + 100))
        .unwrap();
    service
        .store()
        .seed_dig_ready_at(user(1), guild(TEST_GUILD_ID_2), Some(NOW + 100))
        .unwrap();
    let report =
        service.reschedule_all(&[Some(guild(TEST_GUILD_ID)), Some(guild(TEST_GUILD_ID_2))]);
    assert_eq!(
        report.omitted_dig_subscribers,
        [(user(1), guild(TEST_GUILD_ID))]
    );
    assert!(
        service
            .task(task_key(2, TEST_GUILD_ID, ReminderKind::Dig))
            .is_some()
    );
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID_2, ReminderKind::Dig))
            .is_some()
    );
}

#[test]
fn test_recovery_continues_after_guild_subscriber_snapshot_failure() {
    let mut service = fixture();
    set_store_preference(
        service.store(),
        1,
        TEST_GUILD_ID,
        ReminderKind::Trivia,
        true,
    );
    set_store_preference(
        service.store(),
        2,
        TEST_GUILD_ID_2,
        ReminderKind::Wheel,
        true,
    );
    service
        .store()
        .seed_reminder_timestamps(
            user(1),
            guild(TEST_GUILD_ID),
            ReminderTimestamps {
                last_trivia_session: Some(NOW),
                ..ReminderTimestamps::default()
            },
        )
        .unwrap();
    service
        .store()
        .seed_reminder_timestamps(
            user(2),
            guild(TEST_GUILD_ID_2),
            ReminderTimestamps {
                last_wheel_spin: Some(NOW),
                ..ReminderTimestamps::default()
            },
        )
        .unwrap();
    service
        .store()
        .set_bulk_preference_failure(guild(TEST_GUILD_ID), true)
        .unwrap();
    let first = service.reschedule_all(&[Some(guild(TEST_GUILD_ID)), Some(guild(TEST_GUILD_ID_2))]);
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Trivia))
            .is_none()
    );
    assert!(
        service
            .task(task_key(2, TEST_GUILD_ID_2, ReminderKind::Wheel))
            .is_some()
    );
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.failures[0].guild_id, guild(TEST_GUILD_ID));
    assert_eq!(first.failures[0].stage, RecoveryStage::SubscriberSnapshot);

    service
        .store()
        .set_bulk_preference_failure(guild(TEST_GUILD_ID), false)
        .unwrap();
    let retry = service.reschedule_all(&[Some(guild(TEST_GUILD_ID))]);
    assert!(retry.failures.is_empty());
    assert!(
        service
            .task(task_key(1, TEST_GUILD_ID, ReminderKind::Trivia))
            .is_some()
    );
}

#[test]
fn test_arm_preference_schedules_pending_wheel_cooldown() {
    let mut service = fixture();
    let spun_at = NOW - 3 * 3_600;
    service
        .store()
        .seed_reminder_timestamps(
            user(4_242),
            guild(TEST_GUILD_ID),
            ReminderTimestamps {
                last_wheel_spin: Some(spun_at),
                ..ReminderTimestamps::default()
            },
        )
        .unwrap();
    service
        .set_preference(
            user(4_242),
            Some(guild(TEST_GUILD_ID)),
            ReminderKind::Wheel,
            true,
        )
        .unwrap();
    assert!(service.tasks().is_empty());
    service
        .arm_preference(user(4_242), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel)
        .unwrap();
    assert_eq!(
        service
            .task(task_key(4_242, TEST_GUILD_ID, ReminderKind::Wheel))
            .unwrap()
            .due_at,
        spun_at + WHEEL_COOLDOWN_SECONDS
    );
    assert!(
        service
            .task(task_key(4_242, TEST_GUILD_ID, ReminderKind::Wheel))
            .unwrap()
            .delay_seconds
            > 0
    );
}

#[test]
fn test_arm_preference_is_a_noop_with_no_pending_cooldown() {
    let mut service = fixture();
    service
        .store()
        .seed_reminder_timestamps(
            user(4_243),
            guild(TEST_GUILD_ID),
            ReminderTimestamps::default(),
        )
        .unwrap();
    service
        .set_preference(
            user(4_243),
            Some(guild(TEST_GUILD_ID)),
            ReminderKind::Wheel,
            true,
        )
        .unwrap();
    assert_eq!(
        service
            .arm_preference(user(4_243), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel,)
            .unwrap(),
        None
    );
    assert!(
        service
            .task(task_key(4_243, TEST_GUILD_ID, ReminderKind::Wheel))
            .is_none()
    );
}

#[test]
fn test_arm_preference_ignores_elapsed_cooldown() {
    let mut service = fixture();
    service
        .store()
        .seed_reminder_timestamps(
            user(4_244),
            guild(TEST_GUILD_ID),
            ReminderTimestamps {
                last_wheel_spin: Some(NOW - WHEEL_COOLDOWN_SECONDS - 60),
                ..ReminderTimestamps::default()
            },
        )
        .unwrap();
    service
        .set_preference(
            user(4_244),
            Some(guild(TEST_GUILD_ID)),
            ReminderKind::Wheel,
            true,
        )
        .unwrap();
    assert_eq!(
        service
            .arm_preference(user(4_244), Some(guild(TEST_GUILD_ID)), ReminderKind::Wheel,)
            .unwrap(),
        None
    );
    assert!(
        service
            .task(task_key(4_244, TEST_GUILD_ID, ReminderKind::Wheel))
            .is_none()
    );
}

#[test]
fn test_arm_preference_ignores_event_driven_types() {
    let mut service = fixture();
    for kind in [ReminderKind::Lobby, ReminderKind::Betting] {
        assert_eq!(
            service
                .arm_preference(user(4_245), Some(guild(TEST_GUILD_ID)), kind)
                .unwrap(),
            None
        );
    }
    assert!(service.tasks().is_empty());
}

#[test]
fn test_two_concurrent_claimers_have_exactly_one_winner() {
    let store = InMemoryReminderStore::default();
    store
        .add_lobby_target_subscription(user(7_001), user(7_002), guild(TEST_GUILD_ID), 100)
        .unwrap();
    let start = Arc::new(Barrier::new(2));
    let claimers: Vec<_> = (0..2)
        .map(|_| {
            let store = store.clone();
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                store
                    .claim_lobby_target_subscribers(user(7_002), guild(TEST_GUILD_ID), i64::MAX)
                    .unwrap()
            })
        })
        .collect();
    let mut results: Vec<_> = claimers
        .into_iter()
        .map(|claimer| claimer.join().unwrap())
        .collect();
    results.sort_by_key(Vec::len);
    assert_eq!(results, [vec![], vec![user(7_001)]]);
}

#[test]
fn test_reschedule_recovers_pending_pet_warning() {
    let mut service = fixture();
    set_store_preference(service.store(), 8, TEST_GUILD_ID, ReminderKind::Pet, true);
    service
        .store()
        .seed_pet_warning(
            user(8),
            guild(TEST_GUILD_ID),
            PetWarning {
                warning_at: NOW + 600,
                pet_name: "Alpaca".to_owned(),
            },
        )
        .unwrap();

    let report = service.reschedule_all(&[Some(guild(TEST_GUILD_ID))]);
    assert!(report.failures.is_empty());
    let task = service
        .task(task_key(8, TEST_GUILD_ID, ReminderKind::Pet))
        .expect("pet warning recovered");
    assert_eq!(task.due_at, NOW + 600);
    assert_eq!(
        task.message,
        "🦙 **Alpaca** is getting hungry! Check `/pet status` and feed it before it starves."
    );
}

#[test]
fn test_recovering_elapsed_pet_warning_cancels_stale_generation() {
    let mut service = fixture();
    set_store_preference(service.store(), 8, TEST_GUILD_ID, ReminderKind::Pet, true);
    service
        .schedule_pet_reminder(
            user(8),
            Some(guild(TEST_GUILD_ID)),
            NOW + 600,
            "Alpaca",
            Some(true),
        )
        .unwrap();
    service
        .store()
        .seed_pet_warning(
            user(8),
            guild(TEST_GUILD_ID),
            PetWarning {
                warning_at: NOW,
                pet_name: "Alpaca".to_owned(),
            },
        )
        .unwrap();

    let report = service.reschedule_all(&[Some(guild(TEST_GUILD_ID))]);
    assert_eq!(report.cancelled.len(), 1);
    assert!(
        service
            .task(task_key(8, TEST_GUILD_ID, ReminderKind::Pet))
            .is_none()
    );
}
