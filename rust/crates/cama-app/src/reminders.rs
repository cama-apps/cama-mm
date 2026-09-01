//! Deterministic reminder orchestration shared by tests and the Rust runtime.
//!
//! This module owns the policy exercised by `tests/test_reminders.py`: typed
//! preferences, one-shot lobby subscriptions, task replacement/cancellation,
//! restart recovery, and generation-fenced dig reconciliation. Clocks,
//! persistence, timers, and Discord remain explicit ports; the production
//! runtime supplies the existing-schema SQLite adapter and async delivery.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use cama_domain::game_date::{GAME_DAY_SECONDS, next_game_day_start_ts};

pub const TRIVIA_COOLDOWN_SECONDS: i64 = 21_600;
pub const LOWSKILL_RATING_CUTOFF: f64 = 1_400.0;

pub const WHEEL_REMINDER_MESSAGE: &str =
    "Your wheel cooldown has expired! You can `/gamba` again now.";
pub const TRIVIA_REMINDER_MESSAGE: &str =
    "Your trivia cooldown has expired! You can `/trivia` again now.";
pub const DIG_REMINDER_MESSAGE: &str =
    "Your free dig cooldown has expired! You can `/dig go` again now.";

fn next_wheel_reset_at(last_spin: i64) -> i64 {
    next_game_day_start_ts(last_spin).unwrap_or_else(|_| last_spin.saturating_add(GAME_DAY_SECONDS))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReminderCooldowns {
    pub trivia_seconds: i64,
}

impl Default for ReminderCooldowns {
    fn default() -> Self {
        Self {
            trivia_seconds: TRIVIA_COOLDOWN_SECONDS,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UserId(i64);

impl UserId {
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }

    #[must_use]
    pub const fn is_deliverable(self) -> bool {
        self.0 > 0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GuildId(i64);

impl GuildId {
    pub const DIRECT_MESSAGE: Self = Self(0);

    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Normalize the Python `None` guild convention to the persisted guild 0.
    #[must_use]
    pub const fn normalize(value: Option<Self>) -> Self {
        match value {
            Some(guild_id) => guild_id,
            None => Self::DIRECT_MESSAGE,
        }
    }

    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReminderKind {
    Wheel,
    Trivia,
    Betting,
    Dig,
    Lobby,
    Pet,
}

impl ReminderKind {
    pub const ALL: [Self; 6] = [
        Self::Wheel,
        Self::Trivia,
        Self::Betting,
        Self::Dig,
        Self::Lobby,
        Self::Pet,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Wheel => 0,
            Self::Trivia => 1,
            Self::Betting => 2,
            Self::Dig => 3,
            Self::Lobby => 4,
            Self::Pet => 5,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wheel => "wheel",
            Self::Trivia => "trivia",
            Self::Betting => "betting",
            Self::Dig => "dig",
            Self::Lobby => "lobby",
            Self::Pet => "pet",
        }
    }

    #[must_use]
    pub const fn is_event_driven(self) -> bool {
        matches!(self, Self::Lobby | Self::Betting)
    }
}

impl fmt::Display for ReminderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidReminderKind(String);

impl InvalidReminderKind {
    #[must_use]
    pub fn value(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InvalidReminderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid reminder type: {:?}", self.0)
    }
}

impl Error for InvalidReminderKind {}

impl FromStr for ReminderKind {
    type Err = InvalidReminderKind;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "wheel" => Ok(Self::Wheel),
            "trivia" => Ok(Self::Trivia),
            "betting" => Ok(Self::Betting),
            "dig" => Ok(Self::Dig),
            "lobby" => Ok(Self::Lobby),
            "pet" => Ok(Self::Pet),
            invalid => Err(InvalidReminderKind(invalid.to_owned())),
        }
    }
}

/// Parse, validate, and de-duplicate reminder names while preserving input order.
#[must_use]
pub fn valid_reminder_kinds<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<ReminderKind> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter_map(|value| ReminderKind::from_str(value).ok())
        .filter(|kind| seen.insert(*kind))
        .collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReminderPreferences {
    enabled: [bool; 6],
}

impl ReminderPreferences {
    #[must_use]
    pub const fn enabled(self, kind: ReminderKind) -> bool {
        self.enabled[kind.index()]
    }

    pub fn set(&mut self, kind: ReminderKind, enabled: bool) {
        self.enabled[kind.index()] = enabled;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LobbyKind {
    #[default]
    Open,
    LowSkill,
}

impl LobbyKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "🍽️ All You Can Feed",
            Self::LowSkill => "🧀 Whine & Cheese",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerRating {
    pub user_id: UserId,
    pub glicko_rating: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReminderTimestamps {
    pub last_wheel_spin: Option<i64>,
    pub last_trivia_session: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetWarning {
    pub warning_at: i64,
    pub pet_name: String,
}

impl ReminderTimestamps {
    const fn anchor(self, kind: ReminderKind) -> Option<i64> {
        match kind {
            ReminderKind::Wheel => self.last_wheel_spin,
            ReminderKind::Trivia => self.last_trivia_session,
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortError {
    operation: &'static str,
    message: String,
}

impl PortError {
    #[must_use]
    pub fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed: {}", self.operation, self.message)
    }
}

impl Error for PortError {}

/// Persistence boundary. A SQLite adapter must make `claim_lobby_target_subscribers`
/// one atomic read/delete transaction (the Python adapter uses `BEGIN IMMEDIATE`).
pub trait ReminderStore {
    fn get_preferences(
        &self,
        user_id: UserId,
        guild_id: GuildId,
    ) -> Result<ReminderPreferences, PortError>;

    fn set_preference(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        kind: ReminderKind,
        enabled: bool,
    ) -> Result<(), PortError>;

    fn get_enabled_users_for_type(
        &self,
        guild_id: GuildId,
        kind: ReminderKind,
    ) -> Result<Vec<UserId>, PortError>;

    fn get_enabled_users_by_type_bulk(
        &self,
        guild_id: GuildId,
        kinds: &[ReminderKind],
    ) -> Result<BTreeMap<ReminderKind, Vec<UserId>>, PortError>;

    fn add_lobby_target_subscription(
        &self,
        subscriber_id: UserId,
        target_id: UserId,
        guild_id: GuildId,
        created_at_ns: i64,
    ) -> Result<bool, PortError>;

    fn has_lobby_target_subscription(
        &self,
        subscriber_id: UserId,
        target_id: UserId,
        guild_id: GuildId,
    ) -> Result<bool, PortError>;

    fn claim_lobby_target_subscribers(
        &self,
        target_id: UserId,
        guild_id: GuildId,
        created_at_cutoff_ns: i64,
    ) -> Result<Vec<UserId>, PortError>;

    fn get_player_ratings(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
    ) -> Result<Vec<PlayerRating>, PortError>;

    fn get_reminder_timestamps_bulk(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<UserId, ReminderTimestamps>, PortError>;

    fn get_free_dig_ready_at(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        now: i64,
    ) -> Result<Option<i64>, PortError>;

    /// A missing map entry is distinct from an entry whose value is `None`:
    /// missing means the snapshot omitted that subscriber, while `None` means
    /// the subscriber is already ready and any stale task must be cancelled.
    fn get_free_dig_ready_times_bulk(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
        now: i64,
    ) -> Result<BTreeMap<UserId, Option<i64>>, PortError>;

    fn get_pet_warning(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        now: i64,
    ) -> Result<Option<PetWarning>, PortError>;
}

/// Synchronous delivery boundary. Returning from `send_dm` means the attempt
/// has finished; service methods never detach unowned delivery work.
pub trait DiscordPort {
    fn send_dm(&mut self, user_id: UserId, message: &str) -> Result<(), PortError>;
}

pub trait ReminderClock {
    fn now_seconds(&self) -> i64;
    fn now_nanoseconds(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemReminderClock;

impl ReminderClock for SystemReminderClock {
    fn now_seconds(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or_default()
    }

    fn now_nanoseconds(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeDiscordPort;

impl DiscordPort for RuntimeDiscordPort {
    fn send_dm(&mut self, _user_id: UserId, _message: &str) -> Result<(), PortError> {
        Err(PortError::new(
            "send_dm",
            "runtime reminder delivery must use its asynchronous Discord transport",
        ))
    }
}

#[derive(Clone, Default)]
pub struct InMemoryReminderStore {
    state: Arc<Mutex<InMemoryReminderState>>,
}

#[derive(Default)]
struct InMemoryReminderState {
    preferences: BTreeMap<(UserId, GuildId), ReminderPreferences>,
    lobby_targets: BTreeMap<(GuildId, UserId, UserId), i64>,
    ratings: BTreeMap<(GuildId, UserId), Option<f64>>,
    timestamps: BTreeMap<(GuildId, UserId), ReminderTimestamps>,
    dig_ready_times: BTreeMap<(GuildId, UserId), Option<i64>>,
    pet_warnings: BTreeMap<(GuildId, UserId), PetWarning>,
    failed_bulk_preference_guilds: BTreeSet<GuildId>,
}

impl InMemoryReminderStore {
    fn lock_state(&self) -> Result<MutexGuard<'_, InMemoryReminderState>, PortError> {
        self.state
            .lock()
            .map_err(|_| PortError::new("in_memory_reminder_store", "state lock poisoned"))
    }

    pub fn seed_player_rating(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        rating: Option<f64>,
    ) -> Result<(), PortError> {
        self.lock_state()?
            .ratings
            .insert((guild_id, user_id), rating);
        Ok(())
    }

    pub fn seed_reminder_timestamps(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        timestamps: ReminderTimestamps,
    ) -> Result<(), PortError> {
        self.lock_state()?
            .timestamps
            .insert((guild_id, user_id), timestamps);
        Ok(())
    }

    pub fn seed_dig_ready_at(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        ready_at: Option<i64>,
    ) -> Result<(), PortError> {
        self.lock_state()?
            .dig_ready_times
            .insert((guild_id, user_id), ready_at);
        Ok(())
    }

    pub fn omit_dig_ready_at(&self, user_id: UserId, guild_id: GuildId) -> Result<(), PortError> {
        self.lock_state()?
            .dig_ready_times
            .remove(&(guild_id, user_id));
        Ok(())
    }

    pub fn seed_pet_warning(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        warning: PetWarning,
    ) -> Result<(), PortError> {
        self.lock_state()?
            .pet_warnings
            .insert((guild_id, user_id), warning);
        Ok(())
    }

    /// Deterministic adapter failpoint used to prove recovery isolates guilds
    /// and retries the failed guild on a later invocation.
    pub fn set_bulk_preference_failure(
        &self,
        guild_id: GuildId,
        should_fail: bool,
    ) -> Result<(), PortError> {
        let mut state = self.lock_state()?;
        if should_fail {
            state.failed_bulk_preference_guilds.insert(guild_id);
        } else {
            state.failed_bulk_preference_guilds.remove(&guild_id);
        }
        Ok(())
    }
}

impl ReminderStore for InMemoryReminderStore {
    fn get_preferences(
        &self,
        user_id: UserId,
        guild_id: GuildId,
    ) -> Result<ReminderPreferences, PortError> {
        Ok(self
            .lock_state()?
            .preferences
            .get(&(user_id, guild_id))
            .copied()
            .unwrap_or_default())
    }

    fn set_preference(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        kind: ReminderKind,
        enabled: bool,
    ) -> Result<(), PortError> {
        let mut state = self.lock_state()?;
        state
            .preferences
            .entry((user_id, guild_id))
            .or_default()
            .set(kind, enabled);
        Ok(())
    }

    fn get_enabled_users_for_type(
        &self,
        guild_id: GuildId,
        kind: ReminderKind,
    ) -> Result<Vec<UserId>, PortError> {
        Ok(self
            .lock_state()?
            .preferences
            .iter()
            .filter_map(|(&(user_id, stored_guild_id), preferences)| {
                (stored_guild_id == guild_id && preferences.enabled(kind)).then_some(user_id)
            })
            .collect())
    }

    fn get_enabled_users_by_type_bulk(
        &self,
        guild_id: GuildId,
        kinds: &[ReminderKind],
    ) -> Result<BTreeMap<ReminderKind, Vec<UserId>>, PortError> {
        let state = self.lock_state()?;
        if state.failed_bulk_preference_guilds.contains(&guild_id) {
            return Err(PortError::new(
                "get_enabled_users_by_type_bulk",
                format!("broken guild snapshot for {}", guild_id.value()),
            ));
        }

        let unique_kinds: BTreeSet<_> = kinds.iter().copied().collect();
        let mut subscribers: BTreeMap<_, Vec<_>> = unique_kinds
            .iter()
            .copied()
            .map(|kind| (kind, Vec::new()))
            .collect();
        for (&(user_id, stored_guild_id), preferences) in &state.preferences {
            if stored_guild_id != guild_id {
                continue;
            }
            for kind in &unique_kinds {
                if preferences.enabled(*kind) {
                    subscribers.entry(*kind).or_default().push(user_id);
                }
            }
        }
        Ok(subscribers)
    }

    fn add_lobby_target_subscription(
        &self,
        subscriber_id: UserId,
        target_id: UserId,
        guild_id: GuildId,
        created_at_ns: i64,
    ) -> Result<bool, PortError> {
        let mut state = self.lock_state()?;
        let key = (guild_id, target_id, subscriber_id);
        if state.lobby_targets.contains_key(&key) {
            return Ok(false);
        }
        state.lobby_targets.insert(key, created_at_ns);
        Ok(true)
    }

    fn has_lobby_target_subscription(
        &self,
        subscriber_id: UserId,
        target_id: UserId,
        guild_id: GuildId,
    ) -> Result<bool, PortError> {
        Ok(self
            .lock_state()?
            .lobby_targets
            .contains_key(&(guild_id, target_id, subscriber_id)))
    }

    fn claim_lobby_target_subscribers(
        &self,
        target_id: UserId,
        guild_id: GuildId,
        created_at_cutoff_ns: i64,
    ) -> Result<Vec<UserId>, PortError> {
        let mut state = self.lock_state()?;
        let mut claimed: Vec<_> = state
            .lobby_targets
            .iter()
            .filter_map(
                |(&(stored_guild_id, stored_target_id, subscriber_id), &created_at)| {
                    (stored_guild_id == guild_id
                        && stored_target_id == target_id
                        && created_at <= created_at_cutoff_ns)
                        .then_some((created_at, subscriber_id))
                },
            )
            .collect();
        claimed.sort_unstable();
        state
            .lobby_targets
            .retain(|&(stored_guild_id, stored_target_id, _), created_at| {
                stored_guild_id != guild_id
                    || stored_target_id != target_id
                    || *created_at > created_at_cutoff_ns
            });
        Ok(claimed
            .into_iter()
            .map(|(_, subscriber_id)| subscriber_id)
            .collect())
    }

    fn get_player_ratings(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
    ) -> Result<Vec<PlayerRating>, PortError> {
        let state = self.lock_state()?;
        Ok(user_ids
            .iter()
            .filter_map(|&user_id| {
                state
                    .ratings
                    .get(&(guild_id, user_id))
                    .copied()
                    .map(|glicko_rating| PlayerRating {
                        user_id,
                        glicko_rating,
                    })
            })
            .collect())
    }

    fn get_reminder_timestamps_bulk(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<UserId, ReminderTimestamps>, PortError> {
        let state = self.lock_state()?;
        Ok(user_ids
            .iter()
            .filter_map(|&user_id| {
                state
                    .timestamps
                    .get(&(guild_id, user_id))
                    .copied()
                    .map(|timestamps| (user_id, timestamps))
            })
            .collect())
    }

    fn get_free_dig_ready_at(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        _now: i64,
    ) -> Result<Option<i64>, PortError> {
        Ok(self
            .lock_state()?
            .dig_ready_times
            .get(&(guild_id, user_id))
            .copied()
            .flatten())
    }

    fn get_free_dig_ready_times_bulk(
        &self,
        user_ids: &[UserId],
        guild_id: GuildId,
        _now: i64,
    ) -> Result<BTreeMap<UserId, Option<i64>>, PortError> {
        let state = self.lock_state()?;
        Ok(user_ids
            .iter()
            .filter_map(|&user_id| {
                state
                    .dig_ready_times
                    .get(&(guild_id, user_id))
                    .copied()
                    .map(|ready_at| (user_id, ready_at))
            })
            .collect())
    }

    fn get_pet_warning(
        &self,
        user_id: UserId,
        guild_id: GuildId,
        _now: i64,
    ) -> Result<Option<PetWarning>, PortError> {
        Ok(self
            .lock_state()?
            .pet_warnings
            .get(&(guild_id, user_id))
            .cloned())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TaskId(u64);

impl TaskId {
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReminderTaskKey {
    pub user_id: UserId,
    pub guild_id: GuildId,
    pub kind: ReminderKind,
}

impl ReminderTaskKey {
    #[must_use]
    pub const fn new(user_id: UserId, guild_id: GuildId, kind: ReminderKind) -> Self {
        Self {
            user_id,
            guild_id,
            kind,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledReminder {
    pub id: TaskId,
    pub key: ReminderTaskKey,
    pub due_at: i64,
    pub delay_seconds: i64,
    pub message: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeliveryReport {
    pub attempted: Vec<UserId>,
    pub delivered: Vec<UserId>,
    pub failed: Vec<(UserId, PortError)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReminderDelivery {
    pub recipients: Vec<UserId>,
    pub message: String,
}

impl DeliveryReport {
    #[must_use]
    pub fn attempted_count(&self) -> usize {
        self.attempted.len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigReconcileTicket {
    key: ReminderTaskKey,
    reference_now: i64,
    version: u64,
}

impl DigReconcileTicket {
    #[must_use]
    pub const fn key(self) -> ReminderTaskKey {
        self.key
    }

    #[must_use]
    pub const fn reference_now(self) -> i64 {
        self.reference_now
    }

    #[must_use]
    pub const fn version(self) -> u64 {
        self.version
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigReconcileOutcome {
    IgnoredOlderReference,
    Superseded,
    Cancelled(Option<TaskId>),
    Disabled(Option<TaskId>),
    Scheduled(TaskId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryStage {
    SubscriberSnapshot,
    TimestampSnapshot,
    DigReadySnapshot,
    PetWarningSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryFailure {
    pub guild_id: GuildId,
    pub stage: RecoveryStage,
    pub error: PortError,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryReport {
    pub scheduled: Vec<TaskId>,
    pub cancelled: Vec<TaskId>,
    pub omitted_dig_subscribers: Vec<(UserId, GuildId)>,
    pub failures: Vec<RecoveryFailure>,
}

pub struct ReminderService<S, C, D> {
    store: S,
    clock: C,
    discord: D,
    tasks: BTreeMap<ReminderTaskKey, ScheduledReminder>,
    cancelled_tasks: BTreeSet<TaskId>,
    next_task_id: u64,
    dig_reconcile_versions: BTreeMap<ReminderTaskKey, u64>,
    dig_reference_times: BTreeMap<ReminderTaskKey, i64>,
    next_dig_reconcile_version: u64,
    cooldowns: ReminderCooldowns,
}

impl<S, C, D> ReminderService<S, C, D>
where
    S: ReminderStore,
    C: ReminderClock,
    D: DiscordPort,
{
    #[must_use]
    pub fn new(store: S, clock: C, discord: D) -> Self {
        Self {
            store,
            clock,
            discord,
            tasks: BTreeMap::new(),
            cancelled_tasks: BTreeSet::new(),
            next_task_id: 0,
            dig_reconcile_versions: BTreeMap::new(),
            dig_reference_times: BTreeMap::new(),
            next_dig_reconcile_version: 0,
            cooldowns: ReminderCooldowns::default(),
        }
    }

    #[must_use]
    pub const fn with_cooldowns(mut self, cooldowns: ReminderCooldowns) -> Self {
        self.cooldowns = cooldowns;
        self
    }

    #[must_use]
    pub const fn store(&self) -> &S {
        &self.store
    }

    #[must_use]
    pub const fn clock(&self) -> &C {
        &self.clock
    }

    #[must_use]
    pub const fn discord(&self) -> &D {
        &self.discord
    }

    #[must_use]
    pub fn tasks(&self) -> &BTreeMap<ReminderTaskKey, ScheduledReminder> {
        &self.tasks
    }

    #[must_use]
    pub fn task(&self, key: ReminderTaskKey) -> Option<&ScheduledReminder> {
        self.tasks.get(&key)
    }

    #[must_use]
    pub fn was_cancelled(&self, task_id: TaskId) -> bool {
        self.cancelled_tasks.contains(&task_id)
    }

    #[must_use]
    pub fn dig_reconcile_version(&self, key: ReminderTaskKey) -> Option<u64> {
        self.dig_reconcile_versions.get(&key).copied()
    }

    pub fn get_preferences(
        &self,
        user_id: UserId,
        guild_id: Option<GuildId>,
    ) -> Result<ReminderPreferences, PortError> {
        self.store
            .get_preferences(user_id, GuildId::normalize(guild_id))
    }

    pub fn set_preference(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        kind: ReminderKind,
        enabled: bool,
    ) -> Result<bool, PortError> {
        let guild_id = GuildId::normalize(guild_id);
        self.store
            .set_preference(user_id, guild_id, kind, enabled)?;
        if !enabled {
            self.cancel_task(ReminderTaskKey::new(user_id, guild_id, kind));
        }
        Ok(enabled)
    }

    pub fn toggle_preference(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        kind: ReminderKind,
    ) -> Result<bool, PortError> {
        let guild_id = GuildId::normalize(guild_id);
        let enabled = !self.store.get_preferences(user_id, guild_id)?.enabled(kind);
        self.set_preference(user_id, Some(guild_id), kind, enabled)
    }

    pub fn get_lobby_subscriber_ids(
        &self,
        guild_id: Option<GuildId>,
        lobby_kind: Option<LobbyKind>,
    ) -> Result<Vec<UserId>, PortError> {
        let guild_id = GuildId::normalize(guild_id);
        let subscriber_ids = self
            .store
            .get_enabled_users_for_type(guild_id, ReminderKind::Lobby)?;
        if lobby_kind.unwrap_or_default() != LobbyKind::LowSkill {
            return Ok(subscriber_ids);
        }

        Ok(self
            .store
            .get_player_ratings(&subscriber_ids, guild_id)?
            .into_iter()
            .filter_map(|player| {
                player
                    .glicko_rating
                    .is_some_and(|rating| rating < LOWSKILL_RATING_CUTOFF)
                    .then_some(player.user_id)
            })
            .collect())
    }

    pub fn add_lobby_player_subscription(
        &self,
        subscriber_id: UserId,
        target_id: UserId,
        guild_id: Option<GuildId>,
    ) -> Result<bool, PortError> {
        self.store.add_lobby_target_subscription(
            subscriber_id,
            target_id,
            GuildId::normalize(guild_id),
            self.clock.now_nanoseconds(),
        )
    }

    pub fn has_lobby_player_subscription(
        &self,
        subscriber_id: UserId,
        target_id: UserId,
        guild_id: Option<GuildId>,
    ) -> Result<bool, PortError> {
        self.store.has_lobby_target_subscription(
            subscriber_id,
            target_id,
            GuildId::normalize(guild_id),
        )
    }

    pub fn notify_lobby_player_subscribers(
        &mut self,
        target_id: UserId,
        target_display_name: &str,
        guild_id: Option<GuildId>,
        lobby_kind: Option<LobbyKind>,
        join_cutoff_ns: Option<i64>,
    ) -> Result<DeliveryReport, PortError> {
        let guild_id = GuildId::normalize(guild_id);
        let claimed = self.store.claim_lobby_target_subscribers(
            target_id,
            guild_id,
            join_cutoff_ns.unwrap_or_else(|| self.clock.now_nanoseconds()),
        )?;
        let mut seen = BTreeSet::new();
        let subscriber_ids: Vec<_> = claimed
            .into_iter()
            .filter(|subscriber_id| subscriber_id.is_deliverable() && seen.insert(*subscriber_id))
            .collect();
        if subscriber_ids.is_empty() {
            return Ok(DeliveryReport::default());
        }

        let kind = lobby_kind.unwrap_or_default();
        let safe_target_name = escape_discord_text(target_display_name);
        let message = format!(
            "🔔 **{safe_target_name}** just joined {}! This one-time lobby notification has now been used.",
            kind.label()
        );
        Ok(self.send_many(&subscriber_ids, &message))
    }

    pub fn notify_betting_subscribers(
        &mut self,
        guild_id: Option<GuildId>,
        bet_lock_until: i64,
        pending_match_id: Option<i64>,
        lobby_kind: Option<LobbyKind>,
    ) -> Result<DeliveryReport, PortError> {
        let delivery =
            self.prepare_betting_delivery(guild_id, bet_lock_until, pending_match_id, lobby_kind)?;
        Ok(delivery.map_or_else(DeliveryReport::default, |delivery| {
            self.send_many(&delivery.recipients, &delivery.message)
        }))
    }

    pub fn prepare_betting_delivery(
        &self,
        guild_id: Option<GuildId>,
        bet_lock_until: i64,
        pending_match_id: Option<i64>,
        lobby_kind: Option<LobbyKind>,
    ) -> Result<Option<ReminderDelivery>, PortError> {
        let subscribers = self
            .store
            .get_enabled_users_for_type(GuildId::normalize(guild_id), ReminderKind::Betting)?;
        if subscribers.is_empty() {
            return Ok(None);
        }

        let minutes = bet_lock_until
            .saturating_sub(self.clock.now_seconds())
            .max(0)
            / 60;
        let mut match_description = lobby_kind.map_or_else(
            || "match".to_owned(),
            |kind| format!("{} match", kind.label()),
        );
        if let Some(match_id) = pending_match_id {
            match_description.push_str(&format!(" (Match #{match_id})"));
        }
        let message = format!(
            "A new {match_description} has been shuffled! Betting is open for ~{minutes} more minutes. Use `/bet` now!"
        );
        Ok(Some(ReminderDelivery {
            recipients: subscribers,
            message,
        }))
    }

    pub fn schedule_wheel_reminder(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        ready_at: i64,
        preference_enabled: Option<bool>,
    ) -> Result<Option<TaskId>, PortError> {
        self.schedule_cooldown(
            user_id,
            guild_id,
            ReminderKind::Wheel,
            ready_at,
            preference_enabled,
        )
    }

    pub fn schedule_trivia_reminder(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        ready_at: i64,
        preference_enabled: Option<bool>,
    ) -> Result<Option<TaskId>, PortError> {
        self.schedule_cooldown(
            user_id,
            guild_id,
            ReminderKind::Trivia,
            ready_at,
            preference_enabled,
        )
    }

    pub fn schedule_dig_reminder(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        ready_at: i64,
        preference_enabled: Option<bool>,
    ) -> Result<Option<TaskId>, PortError> {
        self.schedule_cooldown(
            user_id,
            guild_id,
            ReminderKind::Dig,
            ready_at,
            preference_enabled,
        )
    }

    pub fn schedule_pet_reminder(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        warning_at: i64,
        pet_name: &str,
        preference_enabled: Option<bool>,
    ) -> Result<Option<TaskId>, PortError> {
        let guild_id = GuildId::normalize(guild_id);
        let key = ReminderTaskKey::new(user_id, guild_id, ReminderKind::Pet);
        let enabled = match preference_enabled {
            Some(enabled) => enabled,
            None => self
                .store
                .get_preferences(user_id, guild_id)?
                .enabled(ReminderKind::Pet),
        };
        if !enabled || warning_at <= self.clock.now_seconds() {
            self.cancel_task(key);
            return Ok(None);
        }
        let message = format!(
            "🦙 **{pet_name}** is getting hungry! Check `/pet status` and feed it before it starves."
        );
        Ok(Some(self.register_task(key, warning_at, &message)))
    }

    pub fn cancel_pet_reminder(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
    ) -> Option<TaskId> {
        self.cancel_task(ReminderTaskKey::new(
            user_id,
            GuildId::normalize(guild_id),
            ReminderKind::Pet,
        ))
    }

    /// Remove one scheduled generation if it is still current. Async runtimes
    /// call this after their wall-clock sleep and only then attempt the DM, so
    /// a replacement or cancellation can never deliver a stale reminder.
    pub fn claim_task(
        &mut self,
        key: ReminderTaskKey,
        task_id: TaskId,
    ) -> Option<ScheduledReminder> {
        if self.tasks.get(&key).map(|task| task.id) != Some(task_id) {
            return None;
        }
        self.tasks.remove(&key)
    }

    pub fn cancel_dig_reminder(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
    ) -> Option<TaskId> {
        self.cancel_task(ReminderTaskKey::new(
            user_id,
            GuildId::normalize(guild_id),
            ReminderKind::Dig,
        ))
    }

    /// Deliver every due task in `(timestamp, task id)` order. Each send is
    /// complete before this method returns, matching the retained Python task.
    pub fn dispatch_due(&mut self) -> Vec<(TaskId, DeliveryReport)> {
        let now = self.clock.now_seconds();
        let mut due: Vec<_> = self
            .tasks
            .values()
            .filter(|task| task.due_at <= now)
            .cloned()
            .collect();
        due.sort_by_key(|task| (task.due_at, task.id));

        due.into_iter()
            .filter_map(|task| {
                let is_current = self
                    .tasks
                    .get(&task.key)
                    .is_some_and(|current| current.id == task.id);
                if !is_current {
                    return None;
                }
                self.tasks.remove(&task.key);
                let report = self.send_many(&[task.key.user_id], &task.message);
                Some((task.id, report))
            })
            .collect()
    }

    /// Reserve a generation for a dig read. An older recovery reference is
    /// rejected before I/O; only the newest generation may commit task state.
    pub fn begin_dig_reconcile(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        reference_now: i64,
    ) -> Option<DigReconcileTicket> {
        let key = ReminderTaskKey::new(user_id, GuildId::normalize(guild_id), ReminderKind::Dig);
        if self
            .dig_reference_times
            .get(&key)
            .is_some_and(|latest| reference_now < *latest)
        {
            return None;
        }

        self.next_dig_reconcile_version = self.next_dig_reconcile_version.saturating_add(1);
        let version = self.next_dig_reconcile_version;
        self.dig_reconcile_versions.insert(key, version);
        self.dig_reference_times.insert(key, reference_now);
        Some(DigReconcileTicket {
            key,
            reference_now,
            version,
        })
    }

    /// Read through the injected authoritative dig store. Keeping this phase
    /// separate lets an async adapter release `&mut self` while it offloads I/O.
    pub fn read_authoritative_dig_ready_at(
        &self,
        ticket: DigReconcileTicket,
    ) -> Result<Option<i64>, PortError> {
        self.store.get_free_dig_ready_at(
            ticket.key.user_id,
            ticket.key.guild_id,
            ticket.reference_now,
        )
    }

    pub fn finish_dig_reconcile(
        &mut self,
        ticket: DigReconcileTicket,
        ready_at: Option<i64>,
        preference_enabled: Option<bool>,
    ) -> Result<DigReconcileOutcome, PortError> {
        if self.dig_reconcile_versions.get(&ticket.key) != Some(&ticket.version) {
            return Ok(DigReconcileOutcome::Superseded);
        }
        if ready_at.is_none_or(|timestamp| timestamp <= ticket.reference_now) {
            return Ok(DigReconcileOutcome::Cancelled(self.cancel_task(ticket.key)));
        }

        let enabled = match preference_enabled {
            Some(enabled) => enabled,
            None => self
                .store
                .get_preferences(ticket.key.user_id, ticket.key.guild_id)?
                .enabled(ReminderKind::Dig),
        };
        if !enabled {
            return Ok(DigReconcileOutcome::Disabled(self.cancel_task(ticket.key)));
        }

        let task_id = self.register_task(
            ticket.key,
            ready_at.unwrap_or_default(),
            DIG_REMINDER_MESSAGE,
        );
        Ok(DigReconcileOutcome::Scheduled(task_id))
    }

    pub fn reconcile_dig_reminder(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        reference_now: Option<i64>,
        preference_enabled: Option<bool>,
    ) -> Result<DigReconcileOutcome, PortError> {
        let reference_now = reference_now.unwrap_or_else(|| self.clock.now_seconds());
        let Some(ticket) = self.begin_dig_reconcile(user_id, guild_id, reference_now) else {
            return Ok(DigReconcileOutcome::IgnoredOlderReference);
        };
        let ready_at = self.read_authoritative_dig_ready_at(ticket)?;
        self.finish_dig_reconcile(ticket, ready_at, preference_enabled)
    }

    pub fn arm_preference(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        kind: ReminderKind,
    ) -> Result<Option<TaskId>, PortError> {
        let guild_id = GuildId::normalize(guild_id);
        let now = self.clock.now_seconds();
        if kind.is_event_driven() {
            return Ok(None);
        }
        if kind == ReminderKind::Pet {
            let Some(warning) = self.store.get_pet_warning(user_id, guild_id, now)? else {
                self.cancel_pet_reminder(user_id, Some(guild_id));
                return Ok(None);
            };
            return self.schedule_pet_reminder(
                user_id,
                Some(guild_id),
                warning.warning_at,
                &warning.pet_name,
                Some(true),
            );
        }
        if kind == ReminderKind::Dig {
            let ready_times =
                self.store
                    .get_free_dig_ready_times_bulk(&[user_id], guild_id, now)?;
            let Some(&ready_at) = ready_times.get(&user_id) else {
                return Ok(None);
            };
            let Some(ticket) = self.begin_dig_reconcile(user_id, Some(guild_id), now) else {
                return Ok(None);
            };
            return self
                .finish_dig_reconcile(ticket, ready_at, Some(true))
                .map(|outcome| match outcome {
                    DigReconcileOutcome::Scheduled(task_id) => Some(task_id),
                    _ => None,
                });
        }

        let timestamps = self
            .store
            .get_reminder_timestamps_bulk(&[user_id], guild_id)?;
        let Some(last_at) = timestamps
            .get(&user_id)
            .and_then(|timestamps| timestamps.anchor(kind))
        else {
            return Ok(None);
        };
        let ready_at = self.cooldown_ready_at(kind, last_at).unwrap_or(last_at);
        if ready_at <= now {
            return Ok(None);
        }
        match kind {
            ReminderKind::Wheel => {
                self.schedule_wheel_reminder(user_id, Some(guild_id), ready_at, Some(true))
            }
            ReminderKind::Trivia => {
                self.schedule_trivia_reminder(user_id, Some(guild_id), ready_at, Some(true))
            }
            _ => Ok(None),
        }
    }

    pub fn reschedule_all(&mut self, guild_ids: &[Option<GuildId>]) -> RecoveryReport {
        let now = self.clock.now_seconds();
        let mut report = RecoveryReport::default();
        let reminder_types = [
            ReminderKind::Wheel,
            ReminderKind::Trivia,
            ReminderKind::Dig,
            ReminderKind::Pet,
        ];

        for guild_id in guild_ids.iter().copied().map(GuildId::normalize) {
            let subscribers_by_type = match self
                .store
                .get_enabled_users_by_type_bulk(guild_id, &reminder_types)
            {
                Ok(subscribers) => subscribers,
                Err(error) => {
                    report.failures.push(RecoveryFailure {
                        guild_id,
                        stage: RecoveryStage::SubscriberSnapshot,
                        error,
                    });
                    continue;
                }
            };

            let mut seen = BTreeSet::new();
            let standard_subscribers: Vec<_> = [ReminderKind::Wheel, ReminderKind::Trivia]
                .into_iter()
                .flat_map(|kind| {
                    subscribers_by_type
                        .get(&kind)
                        .into_iter()
                        .flatten()
                        .copied()
                })
                .filter(|user_id| seen.insert(*user_id))
                .collect();
            let timestamps = match self
                .store
                .get_reminder_timestamps_bulk(&standard_subscribers, guild_id)
            {
                Ok(timestamps) => timestamps,
                Err(error) => {
                    report.failures.push(RecoveryFailure {
                        guild_id,
                        stage: RecoveryStage::TimestampSnapshot,
                        error,
                    });
                    BTreeMap::new()
                }
            };

            for kind in [ReminderKind::Wheel, ReminderKind::Trivia] {
                for &user_id in subscribers_by_type.get(&kind).into_iter().flatten() {
                    let Some(last_at) = timestamps
                        .get(&user_id)
                        .and_then(|timestamps| timestamps.anchor(kind))
                    else {
                        continue;
                    };
                    let ready_at = self.cooldown_ready_at(kind, last_at).unwrap_or(last_at);
                    if ready_at <= now {
                        continue;
                    }
                    let message = match kind {
                        ReminderKind::Wheel => WHEEL_REMINDER_MESSAGE,
                        ReminderKind::Trivia => TRIVIA_REMINDER_MESSAGE,
                        _ => unreachable!("standard reminder kinds are fixed"),
                    };
                    let key = ReminderTaskKey::new(user_id, guild_id, kind);
                    report
                        .scheduled
                        .push(self.register_task(key, ready_at, message));
                }
            }

            for &user_id in subscribers_by_type
                .get(&ReminderKind::Pet)
                .into_iter()
                .flatten()
            {
                match self.store.get_pet_warning(user_id, guild_id, now) {
                    Ok(Some(warning)) if warning.warning_at > now => {
                        if let Ok(Some(task_id)) = self.schedule_pet_reminder(
                            user_id,
                            Some(guild_id),
                            warning.warning_at,
                            &warning.pet_name,
                            Some(true),
                        ) {
                            report.scheduled.push(task_id);
                        }
                    }
                    Ok(_) => {
                        if let Some(task_id) = self.cancel_pet_reminder(user_id, Some(guild_id)) {
                            report.cancelled.push(task_id);
                        }
                    }
                    Err(error) => report.failures.push(RecoveryFailure {
                        guild_id,
                        stage: RecoveryStage::PetWarningSnapshot,
                        error,
                    }),
                }
            }

            let dig_subscribers = subscribers_by_type
                .get(&ReminderKind::Dig)
                .cloned()
                .unwrap_or_default();
            let dig_subscriber_set: BTreeSet<_> = dig_subscribers.iter().copied().collect();
            let stale_keys: Vec<_> = self
                .tasks
                .keys()
                .copied()
                .filter(|key| {
                    key.guild_id == guild_id
                        && key.kind == ReminderKind::Dig
                        && !dig_subscriber_set.contains(&key.user_id)
                })
                .collect();
            for key in stale_keys {
                if let Some(task_id) = self.cancel_stale_dig_for_recovery(key, now) {
                    report.cancelled.push(task_id);
                }
            }

            let ready_times =
                match self
                    .store
                    .get_free_dig_ready_times_bulk(&dig_subscribers, guild_id, now)
                {
                    Ok(ready_times) => ready_times,
                    Err(error) => {
                        report.failures.push(RecoveryFailure {
                            guild_id,
                            stage: RecoveryStage::DigReadySnapshot,
                            error,
                        });
                        continue;
                    }
                };

            for user_id in dig_subscribers {
                let Some(&ready_at) = ready_times.get(&user_id) else {
                    report.omitted_dig_subscribers.push((user_id, guild_id));
                    continue;
                };
                let Some(ticket) = self.begin_dig_reconcile(user_id, Some(guild_id), now) else {
                    continue;
                };
                match self.finish_dig_reconcile(ticket, ready_at, Some(true)) {
                    Ok(DigReconcileOutcome::Scheduled(task_id)) => {
                        report.scheduled.push(task_id);
                    }
                    Ok(
                        DigReconcileOutcome::Cancelled(Some(task_id))
                        | DigReconcileOutcome::Disabled(Some(task_id)),
                    ) => {
                        report.cancelled.push(task_id);
                    }
                    Ok(_) => {}
                    Err(error) => report.failures.push(RecoveryFailure {
                        guild_id,
                        stage: RecoveryStage::DigReadySnapshot,
                        error,
                    }),
                }
            }
        }

        report
    }

    fn schedule_cooldown(
        &mut self,
        user_id: UserId,
        guild_id: Option<GuildId>,
        kind: ReminderKind,
        ready_at: i64,
        preference_enabled: Option<bool>,
    ) -> Result<Option<TaskId>, PortError> {
        let guild_id = GuildId::normalize(guild_id);
        let key = ReminderTaskKey::new(user_id, guild_id, kind);
        let enabled = match preference_enabled {
            Some(enabled) => enabled,
            None => self.store.get_preferences(user_id, guild_id)?.enabled(kind),
        };
        if !enabled {
            if kind == ReminderKind::Dig {
                self.cancel_task(key);
            }
            return Ok(None);
        }
        let message = match kind {
            ReminderKind::Wheel => WHEEL_REMINDER_MESSAGE,
            ReminderKind::Trivia => TRIVIA_REMINDER_MESSAGE,
            ReminderKind::Dig => DIG_REMINDER_MESSAGE,
            _ => unreachable!("only cooldown reminder kinds are scheduled"),
        };
        Ok(Some(self.register_task(key, ready_at, message)))
    }

    fn register_task(&mut self, key: ReminderTaskKey, ready_at: i64, message: &str) -> TaskId {
        self.cancel_task(key);
        self.next_task_id = self.next_task_id.saturating_add(1);
        let task_id = TaskId(self.next_task_id);
        self.tasks.insert(
            key,
            ScheduledReminder {
                id: task_id,
                key,
                due_at: ready_at,
                delay_seconds: ready_at.saturating_sub(self.clock.now_seconds()).max(0),
                message: message.to_owned(),
            },
        );
        task_id
    }

    fn cancel_task(&mut self, key: ReminderTaskKey) -> Option<TaskId> {
        let task_id = self.tasks.remove(&key).map(|task| task.id)?;
        self.cancelled_tasks.insert(task_id);
        Some(task_id)
    }

    fn cancel_stale_dig_for_recovery(
        &mut self,
        key: ReminderTaskKey,
        reference_now: i64,
    ) -> Option<TaskId> {
        if self
            .dig_reference_times
            .get(&key)
            .is_some_and(|latest| reference_now < *latest)
        {
            return None;
        }
        self.next_dig_reconcile_version = self.next_dig_reconcile_version.saturating_add(1);
        self.dig_reconcile_versions
            .insert(key, self.next_dig_reconcile_version);
        self.dig_reference_times.insert(key, reference_now);
        self.cancel_task(key)
    }

    fn send_many(&mut self, subscribers: &[UserId], message: &str) -> DeliveryReport {
        let mut report = DeliveryReport::default();
        for &user_id in subscribers {
            report.attempted.push(user_id);
            match self.discord.send_dm(user_id, message) {
                Ok(()) => report.delivered.push(user_id),
                Err(error) => report.failed.push((user_id, error)),
            }
        }
        report
    }

    const fn cooldown_for(&self, kind: ReminderKind) -> Option<i64> {
        match kind {
            ReminderKind::Wheel => None,
            ReminderKind::Trivia => Some(self.cooldowns.trivia_seconds),
            ReminderKind::Betting | ReminderKind::Dig | ReminderKind::Lobby | ReminderKind::Pet => {
                None
            }
        }
    }

    fn cooldown_ready_at(&self, kind: ReminderKind, last_at: i64) -> Option<i64> {
        match kind {
            ReminderKind::Wheel => Some(next_wheel_reset_at(last_at)),
            _ => self
                .cooldown_for(kind)
                .map(|cooldown| last_at.saturating_add(cooldown)),
        }
    }
}

#[must_use]
pub fn escape_discord_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character == '@' {
            escaped.push('@');
            escaped.push('\u{200b}');
        } else if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
                | '>'
                | '~'
        ) {
            escaped.push('\\');
            escaped.push(character);
        } else {
            escaped.push(character);
        }
    }
    escaped
}

#[cfg(test)]
#[path = "reminders/tests.rs"]
mod tests;
