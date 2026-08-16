//! Adapter-neutral background-worker and gateway-event orchestration.
//!
//! Python remains the production process and the only live Discord/SQLite
//! writer during the migration.  This module captures the scheduling,
//! generation checks, retry boundaries, and single-flight state used by
//! `bot.py` without starting a Tokio runtime or opening a gateway connection.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::economy_event_service::EconomyEventEffects;

pub const DUEL_WORKER_WAKE_SECONDS: u64 = 60;
pub const FIRST_GAME_POOL_WAKE_SECONDS: u64 = 900;
pub const FIRST_GAME_POOL_DAILY_AMOUNT: i64 = 100;
pub const LOBBY_RECONCILE_MAX_ATTEMPTS: usize = 2;
pub const LOBBY_RECONCILE_RETRY_SECONDS: u64 = 1;
pub const THREAD_AUTO_ARCHIVE_MINUTES: u64 = 10_080;
pub const LEGACY_FROGLING_EMOJI_ID: u64 = 1_463_270_458_848_842_003;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LobbyKind {
    Open,
    LowSkill,
}

impl LobbyKind {
    pub const ALL: [Self; 2] = [Self::Open, Self::LowSkill];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "🍽️ All You Can Feed",
            Self::LowSkill => "🧀 Whine & Cheese",
        }
    }
}

// -------------------------------------------------------------------------
// First-game pool funding and durable lobby refreshes
// -------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PoolFunding {
    pub funded_dates: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolTaskError(pub String);

pub trait FirstGamePoolPort {
    fn fund_first_game_pools(
        &mut self,
        guild_id: i64,
        game_date: &str,
        amount_per_lobby: i64,
    ) -> Result<PoolFunding, PoolTaskError>;

    /// Return `true` only after the current lobby generation was edited.
    fn refresh_lobby_generation(
        &mut self,
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<bool, PoolTaskError>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PoolWakeReport {
    pub funding_attempts: Vec<(i64, String, i64)>,
    pub funded_guilds: Vec<i64>,
    pub refresh_attempts: Vec<(i64, LobbyKind)>,
    pub failed_funding_guilds: Vec<i64>,
    pub failed_refreshes: Vec<(i64, LobbyKind)>,
    pub retry_delays: Vec<u64>,
    pub sleep_seconds: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FirstGamePoolState {
    processed_game_dates: BTreeMap<i64, String>,
}

impl FirstGamePoolState {
    #[must_use]
    pub fn processed_date(&self, guild_id: i64) -> Option<&str> {
        self.processed_game_dates.get(&guild_id).map(String::as_str)
    }
}

/// Execute one wake of the pool worker.
///
/// A successful idempotent funding call marks the guild/date as processed.
/// Failed writes do not.  Refresh failures are isolated per lobby kind and
/// guild, and never cause the atomic funding transaction to run twice.
pub fn run_first_game_pool_wake<P: FirstGamePoolPort>(
    state: &mut FirstGamePoolState,
    guild_ids: &[i64],
    game_date: &str,
    port: &mut P,
) -> PoolWakeReport {
    let mut report = PoolWakeReport {
        sleep_seconds: FIRST_GAME_POOL_WAKE_SECONDS,
        ..PoolWakeReport::default()
    };

    for &guild_id in guild_ids {
        if state.processed_date(guild_id) == Some(game_date) {
            continue;
        }
        report.funding_attempts.push((
            guild_id,
            game_date.to_owned(),
            FIRST_GAME_POOL_DAILY_AMOUNT,
        ));
        let funding =
            match port.fund_first_game_pools(guild_id, game_date, FIRST_GAME_POOL_DAILY_AMOUNT) {
                Ok(funding) => funding,
                Err(_) => {
                    report.failed_funding_guilds.push(guild_id);
                    continue;
                }
            };
        state
            .processed_game_dates
            .insert(guild_id, game_date.to_owned());

        if funding.funded_dates.is_empty() {
            continue;
        }
        report.funded_guilds.push(guild_id);
        for lobby_kind in LobbyKind::ALL {
            let mut refreshed = false;
            for attempt in 0..LOBBY_RECONCILE_MAX_ATTEMPTS {
                report.refresh_attempts.push((guild_id, lobby_kind));
                if port
                    .refresh_lobby_generation(guild_id, lobby_kind)
                    .unwrap_or(false)
                {
                    refreshed = true;
                    break;
                }
                if attempt + 1 < LOBBY_RECONCILE_MAX_ATTEMPTS {
                    report
                        .retry_delays
                        .push(LOBBY_RECONCILE_RETRY_SECONDS * (1 << attempt));
                }
            }
            if !refreshed {
                report.failed_refreshes.push((guild_id, lobby_kind));
            }
        }
    }
    report
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelResolution {
    Cache,
    Fetch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbyRefreshSnapshot {
    pub guild_id: i64,
    pub lobby_kind: LobbyKind,
    pub is_open: bool,
    pub message_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub channel_cached: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbyRefreshAction {
    pub guild_id: i64,
    pub lobby_kind: LobbyKind,
    pub expected_message_id: i64,
    pub channel_id: i64,
    pub channel_resolution: ChannelResolution,
}

#[must_use]
pub fn lobby_refresh_action(snapshot: &LobbyRefreshSnapshot) -> Option<LobbyRefreshAction> {
    if !snapshot.is_open {
        return None;
    }
    Some(LobbyRefreshAction {
        guild_id: snapshot.guild_id,
        lobby_kind: snapshot.lobby_kind,
        expected_message_id: snapshot.message_id?,
        channel_id: snapshot.channel_id?,
        channel_resolution: if snapshot.channel_cached {
            ChannelResolution::Cache
        } else {
            ChannelResolution::Fetch
        },
    })
}

// -------------------------------------------------------------------------
// Supervision, worker retention, and isolated durable deliveries
// -------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BodyExit {
    Clean,
    Crash(String),
    Cancelled,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SupervisionReport {
    pub calls: usize,
    pub crash_messages: Vec<String>,
    pub backoff_seconds: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisionCancelled;

/// Replay body exits using Python's 5s exponential backoff capped at 300s.
pub fn supervise<I>(exits: I) -> Result<SupervisionReport, SupervisionCancelled>
where
    I: IntoIterator<Item = BodyExit>,
{
    let mut backoff = 5_u64;
    let mut report = SupervisionReport::default();
    for exit in exits {
        report.calls += 1;
        match exit {
            BodyExit::Clean => return Ok(report),
            BodyExit::Cancelled => return Err(SupervisionCancelled),
            BodyExit::Crash(message) => {
                report.crash_messages.push(message);
                report.backoff_seconds.push(backoff);
                backoff = backoff.saturating_mul(2).min(300);
            }
        }
    }
    Ok(report)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskExit {
    Clean,
    Cancelled,
    Failed(String),
}

#[must_use]
pub fn unexpected_task_exit_log(name: &str, exit: &TaskExit) -> Option<String> {
    match exit {
        TaskExit::Failed(error) => Some(format!(
            "background task {name} exited unexpectedly: {error}"
        )),
        TaskExit::Clean | TaskExit::Cancelled => None,
    }
}

pub trait DuelDeliveryPort {
    fn process_due_challenge(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<(), String>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DuelWakeReport {
    pub attempts: Vec<(i64, i64, i64)>,
    pub failures: Vec<(i64, i64)>,
    pub sleep_seconds: u64,
}

pub fn deliver_due_challenges<P: DuelDeliveryPort>(
    due: &[(i64, i64)],
    now: i64,
    port: &mut P,
) -> DuelWakeReport {
    let mut report = DuelWakeReport {
        sleep_seconds: DUEL_WORKER_WAKE_SECONDS,
        ..DuelWakeReport::default()
    };
    for &(challenge_id, guild_id) in due {
        report.attempts.push((challenge_id, guild_id, now));
        if port
            .process_due_challenge(challenge_id, guild_id, now)
            .is_err()
        {
            report.failures.push((challenge_id, guild_id));
        }
    }
    report
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkerRegistry {
    handles: BTreeMap<String, bool>,
}

impl WorkerRegistry {
    pub fn mark_done(&mut self, name: &str) {
        self.handles.insert(name.to_owned(), true);
    }

    /// Ensure one retained handle for each worker.  Returns newly spawned names.
    pub fn ensure_running<'a, I>(&mut self, names: I) -> Vec<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut spawned = Vec::new();
        for name in names {
            if self.handles.get(name).is_none_or(|done| *done) {
                self.handles.insert(name.to_owned(), false);
                spawned.push(name.to_owned());
            }
        }
        spawned
    }

    #[must_use]
    pub fn is_running(&self, name: &str) -> bool {
        self.handles.get(name) == Some(&false)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerStart {
    pub name: String,
    pub supervised: bool,
    pub observe_exit: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StartupCoordinator {
    pub workers: WorkerRegistry,
    pub lobby_reconciliation_runs: usize,
}

impl StartupCoordinator {
    /// Handle one Discord-ready event. Reconciliation runs on every reconnect,
    /// while retained worker handles suppress overlapping loop instances.
    pub fn on_ready<'a, I>(&mut self, worker_names: I) -> Vec<WorkerStart>
    where
        I: IntoIterator<Item = &'a str>,
    {
        self.lobby_reconciliation_runs += 1;
        self.workers
            .ensure_running(worker_names)
            .into_iter()
            .map(|name| WorkerStart {
                name,
                supervised: true,
                observe_exit: true,
            })
            .collect()
    }
}

// -------------------------------------------------------------------------
// Vanity-tax snapshots and member journal events
// -------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuildMembersSnapshot {
    pub guild_id: i64,
    pub member_ids: Vec<i64>,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VanityRefreshPlan {
    pub snapshots: Vec<GuildMembersSnapshot>,
    pub run_off_event_loop: bool,
}

#[must_use]
pub const fn vanity_refresh_plan(snapshots: Vec<GuildMembersSnapshot>) -> VanityRefreshPlan {
    VanityRefreshPlan {
        snapshots,
        run_off_event_loop: true,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VanityMemberAction {
    Update {
        guild_id: i64,
        member_id: i64,
        nickname: Option<String>,
    },
    Remove {
        guild_id: i64,
        member_id: i64,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VanityMembershipJournal {
    pub actions: Vec<VanityMemberAction>,
}

impl VanityMembershipJournal {
    pub fn joined(&mut self, guild_id: i64, member_id: i64, nickname: Option<&str>) {
        self.actions.push(VanityMemberAction::Update {
            guild_id,
            member_id,
            nickname: nickname.map(str::to_owned),
        });
    }

    pub fn updated(&mut self, guild_id: i64, member_id: i64, nickname: Option<&str>) {
        self.joined(guild_id, member_id, nickname);
    }

    pub fn removed(&mut self, guild_id: i64, member_id: i64) {
        self.actions.push(VanityMemberAction::Remove {
            guild_id,
            member_id,
        });
    }
}

// -------------------------------------------------------------------------
// Lobby message serialization and reconciliation
// -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LobbyMessageKey {
    pub guild_id: i64,
    pub lobby_kind: LobbyKind,
}

#[derive(Debug, Default)]
pub struct SerializedLobbyPublisher {
    locks: Mutex<HashMap<LobbyMessageKey, Arc<Mutex<()>>>>,
}

impl SerializedLobbyPublisher {
    pub fn publish<F>(&self, key: LobbyMessageKey, build_and_edit: F) -> bool
    where
        F: FnOnce() -> bool,
    {
        let gate = {
            let mut locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
            Arc::clone(locks.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
        };
        let _guard = gate.lock().unwrap_or_else(|error| error.into_inner());
        build_and_edit()
    }
}

#[must_use]
pub fn lobby_generation_is_current(
    expected_message_id: Option<i64>,
    stored_message_id: Option<i64>,
) -> bool {
    expected_message_id.is_none() || expected_message_id == stored_message_id
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconcileAttempt {
    pub edit_succeeded: bool,
    pub legacy_reaction_present: bool,
    pub reaction_clear_succeeded: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconcileReport {
    pub attempts: usize,
    pub edit_attempts: usize,
    pub reaction_clear_attempts: usize,
    pub retry_delays: Vec<u64>,
    pub complete: bool,
}

#[must_use]
pub fn reconcile_lobby_message(script: &[ReconcileAttempt]) -> ReconcileReport {
    let mut report = ReconcileReport::default();
    for (index, attempt) in script.iter().take(LOBBY_RECONCILE_MAX_ATTEMPTS).enumerate() {
        report.attempts += 1;
        report.edit_attempts += 1;
        if attempt.legacy_reaction_present {
            report.reaction_clear_attempts += 1;
        }
        let reaction_complete =
            !attempt.legacy_reaction_present || attempt.reaction_clear_succeeded;
        if attempt.edit_succeeded && reaction_complete {
            report.complete = true;
            break;
        }
        if index + 1 < LOBBY_RECONCILE_MAX_ATTEMPTS && index + 1 < script.len() {
            report
                .retry_delays
                .push(LOBBY_RECONCILE_RETRY_SECONDS * (1 << index));
        }
    }
    report
}

// -------------------------------------------------------------------------
// Raw reaction routing without Discord transport
// -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReactionKind {
    Sword,
    Jopacoin,
    Bell,
    Ready,
    LegacyFrogling,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinResult {
    Joined,
    Rejected(JoinRejection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinRejection {
    InFlight,
    AlreadyJoined,
    Full,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawUserSource {
    PayloadMember,
    GuildMemberCache,
    UserCache,
    Rest,
}

#[must_use]
pub const fn resolve_raw_user_source(
    payload_member: bool,
    guild_member: bool,
    cached_user: bool,
) -> RawUserSource {
    if payload_member {
        RawUserSource::PayloadMember
    } else if guild_member {
        RawUserSource::GuildMemberCache
    } else if cached_user {
        RawUserSource::UserCache
    } else {
        RawUserSource::Rest
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReactionInput {
    pub kind: ReactionKind,
    pub message_matches_current_lobby: bool,
    pub lobby_open: bool,
    pub already_in_lobby: bool,
    pub lobby_kind: LobbyKind,
    pub join_result: JoinResult,
    pub post_join_lobby_exists: bool,
    pub lobby_ready: bool,
    pub has_thread: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReactionPlan {
    pub returned_before_discord_lookups: bool,
    pub use_partial_message: bool,
    pub use_full_message_fetch: bool,
    pub remove_reaction: bool,
    pub join_lobby: bool,
    pub leave_lobby: bool,
    pub update_lobby_message: bool,
    pub sync_readycheck: bool,
    pub retain_target_notification: bool,
    pub send_gamba_thread_message: bool,
    pub notify_rally: bool,
    pub notify_ready: bool,
    pub explanation: Option<String>,
}

#[must_use]
pub fn route_reaction_add(input: &ReactionInput) -> ReactionPlan {
    if !input.message_matches_current_lobby {
        return ReactionPlan {
            returned_before_discord_lookups: true,
            ..ReactionPlan::default()
        };
    }
    if !input.lobby_open {
        return ReactionPlan::default();
    }
    match input.kind {
        ReactionKind::LegacyFrogling => ReactionPlan {
            use_partial_message: true,
            remove_reaction: true,
            ..ReactionPlan::default()
        },
        ReactionKind::Jopacoin if !input.already_in_lobby => ReactionPlan {
            send_gamba_thread_message: input.has_thread,
            ..ReactionPlan::default()
        },
        ReactionKind::Sword => {
            let mut plan = ReactionPlan {
                use_partial_message: true,
                join_lobby: true,
                ..ReactionPlan::default()
            };
            match input.join_result {
                JoinResult::Joined => {
                    // The one-shot notification belongs to the committed join,
                    // not the later lobby refetch, so retain it immediately.
                    plan.retain_target_notification = true;
                    if input.post_join_lobby_exists {
                        plan.update_lobby_message = true;
                        plan.sync_readycheck = true;
                        plan.notify_ready = input.lobby_ready;
                        plan.notify_rally = !input.lobby_ready;
                    }
                }
                JoinResult::Rejected(reason) => {
                    plan.remove_reaction = true;
                    plan.explanation = Some(match reason {
                        JoinRejection::InFlight => concat!(
                            "You can't switch lobbies while your current shuffle or draft ",
                            "is in progress."
                        )
                        .to_owned(),
                        JoinRejection::AlreadyJoined => "Already in lobby.".to_owned(),
                        JoinRejection::Full => "Lobby is full.".to_owned(),
                        JoinRejection::Other => "Could not join lobby.".to_owned(),
                    });
                }
            }
            plan
        }
        ReactionKind::Bell | ReactionKind::Ready | ReactionKind::Other | ReactionKind::Jopacoin => {
            ReactionPlan::default()
        }
    }
}

#[must_use]
pub fn route_sword_reaction_remove(
    message_matches_current_lobby: bool,
    lobby_open: bool,
    leave_succeeded: bool,
) -> ReactionPlan {
    if !message_matches_current_lobby {
        return ReactionPlan {
            returned_before_discord_lookups: true,
            ..ReactionPlan::default()
        };
    }
    ReactionPlan {
        use_partial_message: lobby_open,
        leave_lobby: lobby_open,
        update_lobby_message: lobby_open && leave_succeeded,
        sync_readycheck: lobby_open && leave_succeeded,
        ..ReactionPlan::default()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackgroundTaskRegistry {
    retained: BTreeSet<u64>,
}

impl BackgroundTaskRegistry {
    pub fn retain(&mut self, task_id: u64) {
        self.retained.insert(task_id);
    }

    pub fn finish(&mut self, task_id: u64) {
        self.retained.remove(&task_id);
    }

    #[must_use]
    pub fn contains(&self, task_id: u64) -> bool {
        self.retained.contains(&task_id)
    }
}

// -------------------------------------------------------------------------
// Readycheck generation, display, and one-shot completion delivery
// -------------------------------------------------------------------------

pub const READYCHECK_COMPLETE_MESSAGE: &str = concat!(
    "✅ **🍽️ All You Can Feed: 10+ players are ready.** ",
    "You can `/shuffle` now; only ready players will be included."
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiveReadycheckPlayer {
    pub id: i64,
    pub recently_joined: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadycheckState {
    pub message_id: i64,
    pub channel_id: i64,
    pub lobby_ids: BTreeSet<i64>,
    pub reacted: BTreeMap<i64, String>,
    pub declined: BTreeSet<i64>,
}

impl ReadycheckState {
    #[must_use]
    pub fn new(message_id: i64, channel_id: i64, lobby_ids: &[i64]) -> Self {
        Self {
            message_id,
            channel_id,
            lobby_ids: lobby_ids.iter().copied().collect(),
            reacted: BTreeMap::new(),
            declined: BTreeSet::new(),
        }
    }

    /// Apply an add/remove only if the repository generation still matches.
    pub fn apply_reaction(
        &mut self,
        expected_message_id: i64,
        player_id: i64,
        added: bool,
    ) -> bool {
        if self.message_id != expected_message_id {
            return false;
        }
        if added {
            self.reacted.insert(player_id, format!("<@{player_id}>"));
            self.declined.remove(&player_id);
        } else {
            self.reacted.remove(&player_id);
            self.declined.insert(player_id);
        }
        true
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadycheckSyncReport {
    pub state_updated: bool,
    pub display_updated: bool,
    pub physical_reactions_to_clear: BTreeSet<i64>,
    pub cleanup_failures_ignored: BTreeSet<i64>,
    pub completion_reached: bool,
}

/// Reconcile one readycheck generation with the current lobby roster.
pub fn sync_readycheck(
    state: &mut ReadycheckState,
    expected_message_id: i64,
    generation_after_classification: i64,
    players: &[LiveReadycheckPlayer],
    ready_threshold: usize,
    display_edit_succeeds: bool,
    cleanup_failures: &BTreeSet<i64>,
) -> ReadycheckSyncReport {
    if state.message_id != expected_message_id
        || generation_after_classification != expected_message_id
    {
        return ReadycheckSyncReport::default();
    }

    let live_ids = players
        .iter()
        .map(|player| player.id)
        .collect::<BTreeSet<_>>();
    let recent_ids = players
        .iter()
        .filter(|player| player.recently_joined)
        .map(|player| player.id)
        .filter(|player_id| !state.declined.contains(player_id))
        .collect::<BTreeSet<_>>();
    let departed_confirmations = state
        .reacted
        .keys()
        .copied()
        .filter(|player_id| !live_ids.contains(player_id))
        .collect::<BTreeSet<_>>();
    let new_unconfirmed = live_ids
        .difference(&state.lobby_ids)
        .copied()
        .filter(|player_id| !state.reacted.contains_key(player_id))
        .filter(|player_id| !recent_ids.contains(player_id))
        .collect::<BTreeSet<_>>();
    let to_clear = departed_confirmations
        .union(&new_unconfirmed)
        .copied()
        .collect::<BTreeSet<_>>();

    state
        .reacted
        .retain(|player_id, _| live_ids.contains(player_id));
    for player_id in recent_ids {
        state.reacted.insert(player_id, format!("<@{player_id}>"));
    }
    state.lobby_ids = live_ids;

    ReadycheckSyncReport {
        state_updated: true,
        display_updated: display_edit_succeeds,
        physical_reactions_to_clear: to_clear.clone(),
        cleanup_failures_ignored: to_clear.intersection(cleanup_failures).copied().collect(),
        completion_reached: state.reacted.len() >= ready_threshold,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutResult {
    pub status_ok: bool,
    pub mention_ids: Vec<i64>,
    pub jump_url: String,
    pub readycheck_channel_id: i64,
    pub readycheck_message_id: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShortcutPlan {
    pub use_partial_message_to_remove_failed_reaction: bool,
    pub advertise_origin: bool,
    pub content: Option<String>,
    pub allowed_mention_ids: Vec<i64>,
}

#[must_use]
pub fn readycheck_shortcut_plan(
    result: &ShortcutResult,
    lobby_kind: LobbyKind,
    origin_channel_id: Option<i64>,
    current_readycheck_message_id: i64,
) -> ShortcutPlan {
    if !result.status_ok {
        return ShortcutPlan {
            use_partial_message_to_remove_failed_reaction: true,
            ..ShortcutPlan::default()
        };
    }
    let valid_origin = origin_channel_id.is_some_and(|origin| {
        origin != result.readycheck_channel_id
            && current_readycheck_message_id == result.readycheck_message_id
    });
    if !valid_origin || result.mention_ids.is_empty() {
        return ShortcutPlan::default();
    }
    let mentions = result
        .mention_ids
        .iter()
        .map(|id| format!("<@{id}>"))
        .collect::<Vec<_>>()
        .join(" ");
    ShortcutPlan {
        advertise_origin: true,
        content: Some(format!(
            "⚔️ **{} ready check!** {mentions}\n[React in the lobby thread]({})",
            lobby_kind.label(),
            result.jump_url
        )),
        allowed_mention_ids: result.mention_ids.clone(),
        ..ShortcutPlan::default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LobbyReadySnapshot {
    pub open: bool,
    pub player_count: usize,
    pub message_id: Option<i64>,
}

#[derive(Debug, Default)]
pub struct LobbyReadyGate {
    claimed: Mutex<BTreeSet<(i64, LobbyKind)>>,
}

impl LobbyReadyGate {
    /// Atomically validate and claim a lobby-ready announcement.
    pub fn try_claim(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
        ready_threshold: usize,
        initial: LobbyReadySnapshot,
        current: LobbyReadySnapshot,
    ) -> bool {
        if !initial.open
            || !current.open
            || initial.player_count != ready_threshold
            || current.player_count != ready_threshold
            || initial.message_id.is_none()
            || initial.message_id != current.message_id
        {
            return false;
        }
        self.claimed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert((guild_id, lobby_kind))
    }

    pub fn release_after_send_failure(&self, guild_id: i64, lobby_kind: LobbyKind) {
        self.claimed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(guild_id, lobby_kind));
    }
}

#[must_use]
pub const fn lobby_ready_next_step() -> &'static str {
    "Run `/readycheck` before `/shuffle`."
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionChannels {
    pub origin_lookup_succeeded: bool,
    pub origin_available: bool,
    pub fallback_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionTarget {
    Origin,
    Fallback,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadycheckCompletionRegistry {
    notified: BTreeMap<(i64, LobbyKind), i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionAttempt {
    pub guild_id: i64,
    pub lobby_kind: LobbyKind,
    pub readycheck_message_id: i64,
    pub reacted_count_before_resolution: Option<usize>,
    pub reacted_count_before_send: Option<usize>,
    pub ready_threshold: usize,
    pub channels: CompletionChannels,
    pub send_succeeds: bool,
}

impl ReadycheckCompletionRegistry {
    /// Deliver at most once per generation; a failed send never consumes it.
    pub fn deliver(&mut self, attempt: CompletionAttempt) -> Option<CompletionTarget> {
        let key = (attempt.guild_id, attempt.lobby_kind);
        if self.notified.get(&key) == Some(&attempt.readycheck_message_id)
            || attempt
                .reacted_count_before_resolution
                .is_some_and(|count| count < attempt.ready_threshold)
            || attempt
                .reacted_count_before_send
                .is_some_and(|count| count < attempt.ready_threshold)
            || (attempt.reacted_count_before_resolution.is_some()
                && attempt.reacted_count_before_send.is_none())
        {
            return None;
        }
        let target =
            if attempt.channels.origin_lookup_succeeded && attempt.channels.origin_available {
                CompletionTarget::Origin
            } else if attempt.channels.fallback_available {
                CompletionTarget::Fallback
            } else {
                return None;
            };
        if !attempt.send_succeeds {
            return None;
        }
        self.notified.insert(key, attempt.readycheck_message_id);
        Some(target)
    }
}

// -------------------------------------------------------------------------
// Economy-event wake and announcement policy
// -------------------------------------------------------------------------

#[must_use]
pub fn economy_wake_delay(configured_wake: u64, seconds_until_trigger: f64) -> f64 {
    (configured_wake as f64).min(seconds_until_trigger.max(0.0))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnouncementSlot {
    Initial,
    Reminder,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EconomyWakeAction {
    pub announce: bool,
    pub stamp_initial: bool,
    pub stamp_reminder: bool,
    pub alter_disbursement_ballot: bool,
}

#[must_use]
pub const fn economy_wake_action(
    slot: Option<AnnouncementSlot>,
    announcement_delivered: bool,
) -> EconomyWakeAction {
    EconomyWakeAction {
        announce: slot.is_some(),
        stamp_initial: matches!(slot, Some(AnnouncementSlot::Initial)) && announcement_delivered,
        stamp_reminder: matches!(slot, Some(AnnouncementSlot::Reminder)) && announcement_delivered,
        // Rolling-band events restrict new reserve votes through the service;
        // they never delete or pause the durable proposal here.
        alter_disbursement_ballot: false,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisbursementBallot {
    pub proposal_id: i64,
    pub reserve_jc: i64,
    pub votes: BTreeMap<String, usize>,
}

impl DisbursementBallot {
    pub fn apply_severe_deflationary_restriction(&mut self) {
        self.reserve_jc = 0;
    }

    pub fn add_vote(&mut self, choice: &str, restriction_active: bool) -> bool {
        if restriction_active {
            return false;
        }
        *self.votes.entry(choice.to_owned()).or_default() += 1;
        true
    }

    #[must_use]
    pub fn total_votes(&self) -> usize {
        self.votes.values().sum()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EconomyEvent {
    pub name: Option<String>,
    pub severity: u8,
    pub direction: String,
    pub announcement: Option<String>,
    pub ends_at: Option<i64>,
    pub effects: EconomyEventEffects,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PublicEconomyEventEmbed {
    pub title: String,
    pub description: String,
    pub color: u32,
    pub fields: Vec<(String, String)>,
    pub thumbnail_url: Option<String>,
    pub footer: String,
}

fn roman_level(level: u8) -> &'static str {
    match level {
        1 => "I",
        2 => "II",
        3 => "III",
        4 => "IV",
        5 => "V",
        _ => "I",
    }
}

fn relative_effect(multiplier: f64, lower: &str, higher: &str) -> Option<String> {
    let change = (multiplier - 1.0) * 100.0;
    if change.abs() < 0.05 {
        return None;
    }
    let magnitude = (change.abs() * 10.0).round() / 10.0;
    let formatted = if magnitude.fract() == 0.0 {
        format!("{}%", magnitude as i64)
    } else {
        format!("{magnitude:.1}%")
    };
    Some(format!(
        "**{formatted} {}**",
        if change < 0.0 { lower } else { higher }
    ))
}

fn format_jc(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut result = String::new();
    for (index, character) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(character);
    }
    if negative {
        format!("-{result}")
    } else {
        result
    }
}

fn event_effect_lines(effects: &EconomyEventEffects) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(effect) = relative_effect(effects.reward_multiplier, "lower", "higher") {
        lines.push(format!("🎁 Generated rewards are {effect}."));
    }
    let mut gamba = Vec::new();
    if let Some(effect) = relative_effect(effects.gamba_win_multiplier, "lower", "higher") {
        gamba.push(format!("wins {effect}"));
    }
    if let Some(effect) = relative_effect(effects.gamba_loss_multiplier, "softer", "harsher") {
        gamba.push(format!("losses {effect}"));
    }
    if !gamba.is_empty() {
        lines.push(format!("🎰 Gamba: {}.", gamba.join(", ")));
    }
    if let Some(effect) = relative_effect(effects.bet_payout_multiplier, "lower", "higher") {
        lines.push(format!("⚔️ Match-bet payouts are {effect}."));
    }
    let mut prediction = Vec::new();
    if let Some(effect) = relative_effect(effects.prediction_payout_multiplier, "lower", "higher") {
        prediction.push(format!("resolution payouts {effect}"));
    }
    if let Some(effect) = relative_effect(effects.prediction_depth_multiplier, "thinner", "deeper")
    {
        prediction.push(format!("liquidity {effect}"));
    }
    if effects.prediction_spread_ticks_delta != 0 {
        prediction.push(format!(
            "spreads **{} ticks {}**",
            effects.prediction_spread_ticks_delta.abs(),
            if effects.prediction_spread_ticks_delta > 0 {
                "wider"
            } else {
                "tighter"
            }
        ));
    }
    if !prediction.is_empty() {
        lines.push(format!("📈 Prediction markets: {}.", prediction.join(", ")));
    }
    if effects.reserve_voting_disabled {
        lines.push("🏛️ Jopacoin Reserve allocation voting is suspended.".to_owned());
    }
    if lines.is_empty() {
        lines.push("⚖️ Payouts and market conditions remain at their normal strength.".to_owned());
    }
    lines
}

fn immediate_effect_text(effects: &EconomyEventEffects) -> String {
    let mut actions = Vec::new();
    if effects.reserve_burn_jc != 0 {
        actions.push(format!(
            "**{} JC** burned from the Jopa Reserve",
            format_jc(effects.reserve_burn_jc)
        ));
    }
    if effects.wallet_burn_jc != 0 {
        actions.push(format!(
            "**{} JC** burned from positive wallets",
            format_jc(effects.wallet_burn_jc)
        ));
    }
    if effects.reserve_release_jc != 0 {
        actions.push(format!(
            "**{} JC** redistributed from the Jopa Reserve",
            format_jc(effects.reserve_release_jc)
        ));
    }
    if actions.is_empty() {
        concat!(
            "No JC moved when this spell activated. Its pressure arrives through ",
            "adjusted payouts and liquidity as activity settles."
        )
        .to_owned()
    } else {
        format!("The spell took immediate hold: {}.", actions.join("; "))
    }
}

#[must_use]
pub fn build_public_economy_event_embed(
    event: &EconomyEvent,
    icon_url: Option<&str>,
) -> PublicEconomyEventEmbed {
    let name = event.name.as_deref().unwrap_or("Economic Event");
    let (icon, color, edict) = match event.direction.as_str() {
        "deflationary" => ("🌑", 0xD9_4B_4B, "A deflationary edict grips the server."),
        "boon" => ("✨", 0x43_B5_81, "A boon washes over the server."),
        _ => (
            "🕰️",
            0x7F_8C_8D,
            "The market bends without changing course.",
        ),
    };
    let announcement = event.announcement.as_deref().unwrap_or("").trim();
    let flavor = announcement
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .unwrap_or(edict);
    let expiry = event.ends_at.map_or_else(
        || "Active until the next 10 AM Pacific rollover.".to_owned(),
        |ends_at| format!("Active until <t:{ends_at}:R>."),
    );
    PublicEconomyEventEmbed {
        title: format!("{icon} {name} — Level {}", roman_level(event.severity)),
        description: format!("*{flavor}*\n\n**{edict}** {expiry}"),
        color,
        fields: vec![
            (
                "The Spell Takes Effect".to_owned(),
                event_effect_lines(&event.effects).join("\n"),
            ),
            (
                "Immediate Impact".to_owned(),
                immediate_effect_text(&event.effects),
            ),
        ],
        thumbnail_url: icon_url.map(str::to_owned),
        footer: concat!(
            "The treasury watches. Bands follow the rolling 7-day change ",
            "around a 2% annual target."
        )
        .to_owned(),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EconomyAnnouncementPlan {
    pub sent: bool,
    pub embed: Option<PublicEconomyEventEmbed>,
    pub warning: Option<String>,
}

#[must_use]
pub fn economy_announcement_plan(
    guild_id: i64,
    event: &EconomyEvent,
    prediction_cog_loaded: bool,
    icon_lookup: Result<Option<String>, String>,
) -> EconomyAnnouncementPlan {
    if !prediction_cog_loaded {
        return EconomyAnnouncementPlan {
            warning: Some(format!(
                "economy event announcement skipped for guild={guild_id}: \
                 PredictionCommands cog not loaded"
            )),
            ..EconomyAnnouncementPlan::default()
        };
    }
    match icon_lookup {
        Ok(icon) => EconomyAnnouncementPlan {
            sent: true,
            embed: Some(build_public_economy_event_embed(event, icon.as_deref())),
            warning: None,
        },
        Err(_) => EconomyAnnouncementPlan {
            sent: true,
            embed: Some(build_public_economy_event_embed(event, None)),
            warning: Some(format!(
                "economy event icon lookup failed for event={} guild={guild_id}",
                event.name.as_deref().unwrap_or("")
            )),
        },
    }
}

// -------------------------------------------------------------------------
// Reminder recovery single-flight lifecycle
// -------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryStart {
    pub token: u64,
    pub guild_ids: Vec<i64>,
    pub newly_started: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReminderRecovery {
    current: Option<(u64, Vec<i64>)>,
    next_token: u64,
    pub warnings: Vec<String>,
}

impl ReminderRecovery {
    pub fn start(&mut self, guild_ids: &[i64]) -> RecoveryStart {
        if let Some((token, snapshot)) = &self.current {
            return RecoveryStart {
                token: *token,
                guild_ids: snapshot.clone(),
                newly_started: false,
            };
        }
        self.next_token += 1;
        let snapshot = guild_ids.to_vec();
        self.current = Some((self.next_token, snapshot.clone()));
        RecoveryStart {
            token: self.next_token,
            guild_ids: snapshot,
            newly_started: true,
        }
    }

    pub fn finish(&mut self, token: u64, error: Option<&str>) {
        if self.current.as_ref().map(|(current, _)| *current) != Some(token) {
            return;
        }
        if let Some(error) = error {
            self.warnings
                .push(format!("Reminder recovery sweep failed: {error}"));
        }
        self.current = None;
    }

    #[must_use]
    pub fn current_token(&self) -> Option<u64> {
        self.current.as_ref().map(|(token, _)| *token)
    }
}

// -------------------------------------------------------------------------
// Digest timing, biggest-mover charting, and refresh summary publication
// -------------------------------------------------------------------------

#[must_use]
pub fn next_digest_run(now: DateTime<Utc>, anchor_hours: &[u32]) -> Option<DateTime<Utc>> {
    let date = now.date_naive();
    (0_i64..=1)
        .flat_map(|day_offset| {
            anchor_hours.iter().filter_map(move |&hour| {
                if hour > 23 {
                    return None;
                }
                let day = date.checked_add_signed(Duration::days(day_offset))?;
                Utc.with_ymd_and_hms(day.year(), day.month(), day.day(), hour, 0, 0)
                    .single()
            })
        })
        .filter(|candidate| *candidate > now)
        .min()
}

// chrono's date accessors are trait methods.
use chrono::Datelike;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigestMarket {
    pub prediction_id: i64,
    pub current_price: i32,
    pub previous_price: i32,
    pub recent_volume: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigestPlan {
    pub field_order: Vec<i64>,
    pub chart_market_id: Option<i64>,
    pub attachment_filename: Option<String>,
    pub image_url: Option<String>,
    pub description: Option<String>,
}

#[must_use]
pub fn build_digest_plan(markets: &[DigestMarket], chart_filename: Option<&str>) -> DigestPlan {
    let mut sorted = markets.to_vec();
    sorted.sort_by_key(|market| std::cmp::Reverse(market.recent_volume));
    let biggest = markets
        .iter()
        .filter(|market| market.current_price != market.previous_price)
        .max_by_key(|market| {
            (i64::from(market.current_price) - i64::from(market.previous_price)).abs()
        });
    let Some(biggest) = biggest else {
        return DigestPlan {
            field_order: sorted.iter().map(|market| market.prediction_id).collect(),
            ..DigestPlan::default()
        };
    };
    let delta = i64::from(biggest.current_price) - i64::from(biggest.previous_price);
    let arrow = if delta > 0 { '↑' } else { '↓' };
    DigestPlan {
        field_order: sorted.iter().map(|market| market.prediction_id).collect(),
        chart_market_id: Some(biggest.prediction_id),
        attachment_filename: chart_filename.map(str::to_owned),
        image_url: chart_filename.map(|filename| format!("attachment://{filename}")),
        description: Some(format!(
            "**Biggest mover:** #{} {arrow}{}",
            biggest.prediction_id,
            delta.abs()
        )),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefreshSummary {
    pub skipped: bool,
    pub old_price: i32,
    pub new_price: i32,
    pub trade_count: usize,
    pub total_volume: i64,
    pub yes_volume: i64,
    pub no_volume: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefreshAction {
    RefreshMarketEmbed,
    UnarchiveThread { auto_archive_minutes: u64 },
    SendDailySummary(String),
}

#[must_use]
pub fn process_refresh_plan(
    summary: &RefreshSummary,
    thread_id: Option<i64>,
    current_market_status: Option<&str>,
) -> Vec<RefreshAction> {
    if summary.skipped {
        return Vec::new();
    }
    let mut actions = vec![RefreshAction::RefreshMarketEmbed];
    if summary.trade_count == 0 || thread_id.is_none() || current_market_status != Some("open") {
        return actions;
    }
    actions.push(RefreshAction::UnarchiveThread {
        auto_archive_minutes: THREAD_AUTO_ARCHIVE_MINUTES,
    });
    actions.push(RefreshAction::SendDailySummary(format!(
        "📈 **Daily refresh** — volume {} contracts ({} YES / {} NO), YES {}% → {}%",
        summary.total_volume,
        summary.yes_volume,
        summary.no_volume,
        summary.old_price,
        summary.new_price
    )));
    actions
}

#[cfg(test)]
mod tests;
