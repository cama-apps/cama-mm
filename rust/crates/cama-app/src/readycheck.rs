//! Transport-neutral ready-check lifecycle and lobby timestamp policy.
//!
//! The Python bot remains the production authority. This module makes the
//! state transitions that currently sit behind `/readycheck` explicit and
//! executable without depending on Discord SDK objects: live membership,
//! generation checks, explicit declines, stale-check recovery, cooldowns,
//! completion idempotency, and ordered transport operations are all typed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::readycheck_repository::PersistedLobbyState;

use crate::dedicated_lobby_channel::{ChannelId, GuildId, LobbyScope, MessageId, UserId};
use crate::embeds::LobbyKind;

pub const FULL_LOBBY_SIZE: usize = 10;
/// A ready check against a near-empty lobby wastes everyone's react; this is
/// a fixed floor rather than a per-guild setting for now.
pub const MINIMUM_READYCHECK_PLAYERS: usize = 8;
pub const READYCHECK_COOLDOWN_SECONDS: f64 = 120.0;
pub const READYCHECK_STALE_SECONDS: f64 = 30.0 * 60.0;
pub const RECENT_JOIN_GRACE_SECONDS: f64 = 10.0 * 60.0;
pub const READY_LOBBY_RECOMMENDATION: &str = "Run `/readycheck` before `/shuffle`.";

#[must_use]
pub fn readycheck_description(player_count: usize, ready_threshold: usize) -> String {
    let mut description = format!("**{player_count}** players in lobby");
    if player_count < ready_threshold {
        let shortfall = ready_threshold - player_count;
        description.push_str(&format!(" — need {shortfall} more for a full game"));
    }
    description
}

#[must_use]
pub fn ready_announcement(kind: LobbyKind, ready_threshold: usize) -> String {
    format!(
        "✅ **{}: {ready_threshold}+ players are ready.** You can `/shuffle` now; only ready players will be included.",
        kind.label()
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LobbyStatus {
    #[default]
    Open,
    Closed,
}

impl LobbyStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }

    #[must_use]
    pub fn from_storage(value: &str) -> Self {
        if value.eq_ignore_ascii_case("open") {
            Self::Open
        } else {
            Self::Closed
        }
    }
}

/// Ready-check-facing lobby model. Legacy conditional membership can be read
/// and cleared, but it never contributes to readiness and cannot accept joins.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadyLobby {
    pub scope: LobbyScope,
    pub created_by: UserId,
    pub created_at: String,
    pub players: BTreeSet<UserId>,
    pub legacy_conditional_players: BTreeSet<UserId>,
    pub player_join_times: BTreeMap<UserId, f64>,
    pub status: LobbyStatus,
    pub thread_id: Option<ChannelId>,
}

impl ReadyLobby {
    #[must_use]
    pub fn new(scope: LobbyScope, created_by: UserId, created_at: impl Into<String>) -> Self {
        Self {
            scope,
            created_by,
            created_at: created_at.into(),
            players: BTreeSet::new(),
            legacy_conditional_players: BTreeSet::new(),
            player_join_times: BTreeMap::new(),
            status: LobbyStatus::Open,
            thread_id: None,
        }
    }

    /// Add a regular player at a caller-supplied timestamp. Moving a legacy
    /// conditional player preserves an earlier timestamp rather than
    /// pretending they just arrived.
    pub fn add_player_at(&mut self, player_id: UserId, joined_at: f64) -> bool {
        if self.status != LobbyStatus::Open || self.players.contains(&player_id) {
            return false;
        }
        self.legacy_conditional_players.remove(&player_id);
        self.players.insert(player_id);
        self.player_join_times.entry(player_id).or_insert(joined_at);
        true
    }

    /// The deprecated conditional queue is deliberately closed.
    pub const fn add_conditional_player(&mut self, _player_id: UserId) -> bool {
        false
    }

    pub fn remove_player(&mut self, player_id: UserId) -> bool {
        if !self.players.remove(&player_id) {
            return false;
        }
        self.player_join_times.remove(&player_id);
        true
    }

    pub fn remove_conditional_player(&mut self, player_id: UserId) -> bool {
        if !self.legacy_conditional_players.remove(&player_id) {
            return false;
        }
        self.player_join_times.remove(&player_id);
        true
    }

    #[must_use]
    pub fn total_count(&self) -> usize {
        self.players.len()
    }

    #[must_use]
    pub fn is_ready(&self, threshold: usize) -> bool {
        self.total_count() >= threshold
    }

    #[must_use]
    pub fn to_document(&self) -> LobbyDocument {
        LobbyDocument {
            lobby_id: self.scope.lobby_id(),
            guild_id: self.scope.guild_id.0,
            kind: self.scope.kind,
            created_by: self.created_by.0,
            created_at: self.created_at.clone(),
            players: self.players.iter().map(|player_id| player_id.0).collect(),
            conditional_players: Vec::new(),
            player_join_times: self
                .player_join_times
                .iter()
                .map(|(player_id, timestamp)| (player_id.0.to_string(), *timestamp))
                .collect(),
            status: self.status,
        }
    }

    #[must_use]
    pub fn from_document(document: LobbyDocument) -> Self {
        let players = document
            .players
            .into_iter()
            .map(UserId)
            .collect::<BTreeSet<_>>();
        let player_join_times = document
            .player_join_times
            .into_iter()
            .filter_map(|(player_id, timestamp)| {
                let player_id = player_id.parse::<i64>().ok().map(UserId)?;
                players
                    .contains(&player_id)
                    .then_some((player_id, timestamp))
            })
            .collect();
        Self {
            scope: LobbyScope::new(GuildId(document.guild_id), document.kind),
            created_by: UserId(document.created_by),
            created_at: document.created_at,
            players,
            // Python intentionally drops the deprecated queue on model decode.
            legacy_conditional_players: BTreeSet::new(),
            player_join_times,
            status: document.status,
            thread_id: None,
        }
    }

    #[must_use]
    pub fn to_persisted(&self) -> PersistedLobbyState {
        PersistedLobbyState {
            lobby_id: self.scope.lobby_id(),
            guild_id: self.scope.guild_id.0,
            players: self.players.iter().map(|player_id| player_id.0).collect(),
            conditional_players: self
                .legacy_conditional_players
                .iter()
                .map(|player_id| player_id.0)
                .collect(),
            status: self.status.as_str().to_owned(),
            created_by: self.created_by.0,
            created_at: self.created_at.clone(),
            message_id: None,
            channel_id: None,
            thread_id: self.thread_id.map(|thread_id| thread_id.0),
            embed_message_id: None,
            origin_channel_id: None,
            player_join_times: self
                .player_join_times
                .iter()
                .map(|(player_id, timestamp)| (player_id.0, *timestamp))
                .collect(),
        }
    }

    #[must_use]
    pub fn from_persisted(state: PersistedLobbyState) -> Self {
        let kind = if state.lobby_id
            == LobbyScope::new(GuildId::DEFAULT, LobbyKind::LowSkill).lobby_id()
        {
            LobbyKind::LowSkill
        } else {
            LobbyKind::Open
        };
        let players = state
            .players
            .into_iter()
            .map(UserId)
            .collect::<BTreeSet<_>>();
        Self {
            scope: LobbyScope::new(GuildId(state.guild_id), kind),
            created_by: UserId(state.created_by),
            created_at: state.created_at,
            legacy_conditional_players: state.conditional_players.into_iter().map(UserId).collect(),
            player_join_times: state
                .player_join_times
                .into_iter()
                .filter_map(|(player_id, timestamp)| {
                    let player_id = UserId(player_id);
                    players
                        .contains(&player_id)
                        .then_some((player_id, timestamp))
                })
                .collect(),
            players,
            status: LobbyStatus::from_storage(&state.status),
            thread_id: state.thread_id.map(ChannelId),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LobbyDocument {
    pub lobby_id: i64,
    pub guild_id: i64,
    pub kind: LobbyKind,
    pub created_by: i64,
    pub created_at: String,
    pub players: Vec<i64>,
    pub conditional_players: Vec<i64>,
    pub player_join_times: BTreeMap<String, f64>,
    pub status: LobbyStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityKind {
    Game,
    Custom,
    Streaming,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberActivity {
    pub kind: ActivityKind,
    pub name: Option<String>,
}

impl MemberActivity {
    #[must_use]
    pub fn game(name: impl Into<String>) -> Self {
        Self {
            kind: ActivityKind::Game,
            name: Some(name.into()),
        }
    }
}

/// Discord activity names are untrusted presentation strings. Matching is
/// intentionally case-insensitive and accepts the embedded `Dota 2` phrase,
/// matching Python for game and generic activity objects alike.
#[must_use]
pub fn is_playing_dota(activities: &[MemberActivity]) -> bool {
    activities.iter().any(|activity| {
        activity
            .name
            .as_deref()
            .is_some_and(|name| name.to_lowercase().contains("dota 2"))
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReadinessGroup {
    Ready,
    PlayingDota,
    Recent,
    #[default]
    Afk,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadycheckPlayerData {
    pub name: String,
    pub group: ReadinessGroup,
    pub signals: String,
    pub joined_at: Option<i64>,
}

impl ReadycheckPlayerData {
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            group: ReadinessGroup::Afk,
            signals: String::new(),
            joined_at: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReadycheckGeneration {
    pub message_id: MessageId,
    pub channel_id: ChannelId,
    pub lobby_ids: BTreeSet<UserId>,
    pub player_data: BTreeMap<UserId, ReadycheckPlayerData>,
    pub reacted: BTreeMap<UserId, String>,
    pub declined: BTreeSet<UserId>,
    pub created_at: f64,
    pub completion_notified: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReadycheckStateInput {
    pub message_id: MessageId,
    pub channel_id: ChannelId,
    pub lobby_ids: BTreeSet<UserId>,
    pub player_data: BTreeMap<UserId, ReadycheckPlayerData>,
    pub created_at: Option<f64>,
    pub initial_reacted: BTreeMap<UserId, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmationSnapshot {
    pub message_id: MessageId,
    pub confirmed_ids: BTreeSet<UserId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LobbyReadycheckSnapshot {
    pub player_ids: Vec<UserId>,
    pub player_join_times: BTreeMap<UserId, f64>,
    pub readycheck: Option<ConfirmationSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadycheckRosterUpdate {
    pub removed_confirmations: BTreeSet<UserId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationMode {
    New,
    Refresh,
    ReplaceStale,
    RecoverMissing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryRequirement {
    Required,
    BestEffort,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadycheckTransportOperation {
    EditReadycheck {
        channel_id: ChannelId,
        message_id: MessageId,
        requirement: DeliveryRequirement,
    },
    DeleteReadycheck {
        channel_id: ChannelId,
        message_id: MessageId,
        requirement: DeliveryRequirement,
    },
    RemoveDepartedReaction {
        channel_id: ChannelId,
        message_id: MessageId,
        player_id: UserId,
        emoji: &'static str,
        requirement: DeliveryRequirement,
    },
    SyncLobbyDisplays {
        scope: LobbyScope,
        requirement: DeliveryRequirement,
    },
    PostReadycheck {
        thread_id: ChannelId,
        requirement: DeliveryRequirement,
    },
    ReactToPostedReadycheck {
        emoji: &'static str,
        requirement: DeliveryRequirement,
    },
    MirrorPing {
        channel_id: ChannelId,
        mentioned_users: BTreeSet<UserId>,
        requirement: DeliveryRequirement,
    },
    NotifyShuffleReady {
        channel_id: ChannelId,
        requirement: DeliveryRequirement,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadycheckTransportPlan {
    pub operations: Vec<ReadycheckTransportOperation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExistingMessageState {
    Available,
    Missing,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReadycheckCommandRequest {
    pub guild_id: Option<GuildId>,
    pub kind: LobbyKind,
    pub invoker_id: UserId,
    pub invocation_channel_id: ChannelId,
    pub now: f64,
    pub confirm_invoker: bool,
    pub existing_message: ExistingMessageState,
    /// A shuffle/draft reservation prevents destructive stale pruning.
    pub reserved_players: BTreeSet<UserId>,
    /// A ready check against too small a lobby wastes everyone's react —
    /// callers supply the floor so production and tests can differ.
    pub minimum_players: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadycheckCommandFailure {
    NoGuild,
    NoLobby,
    TooFewPlayers { minimum: usize, current: usize },
    NotMember,
    NoThread,
    Cooldown { retry_after_seconds: u64 },
    PublicationInFlight,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadycheckPermit {
    token: u64,
    pub scope: LobbyScope,
    pub expected_message_id: Option<MessageId>,
    pub invoker_id: UserId,
    pub confirm_invoker: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReadycheckCommandPlan {
    pub scope: LobbyScope,
    pub mode: PublicationMode,
    pub permit: Option<ReadycheckPermit>,
    pub pruned_players: BTreeSet<UserId>,
    pub mention_ids: BTreeSet<UserId>,
    pub transport: ReadycheckTransportPlan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitPublicationResult {
    Applied,
    StalePermit,
}

#[derive(Clone, Debug)]
struct PendingPublication {
    token: u64,
    expected_message_id: Option<MessageId>,
}

#[derive(Clone, Debug, Default)]
struct ScopedState {
    lobby: Option<ReadyLobby>,
    readycheck: Option<ReadycheckGeneration>,
    last_command_at: Option<f64>,
    pending_publication: Option<PendingPublication>,
}

#[derive(Debug, Default)]
struct ServiceState {
    scopes: BTreeMap<LobbyScope, ScopedState>,
    next_token: u64,
}

/// All lobby and ready-check state shares one mutex so membership mutations,
/// reaction transitions, generation replacement, and composite snapshots are
/// linearizable. Poison recovery preserves availability after a panicking
/// worker; the recovered state is still protected by the mutex.
#[derive(Clone, Debug, Default)]
pub struct ReadycheckService {
    state: Arc<Mutex<ServiceState>>,
}

impl ReadycheckService {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, ServiceState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn put_lobby(&self, lobby: ReadyLobby) {
        let scope = lobby.scope;
        self.lock().scopes.entry(scope).or_default().lobby = Some(lobby);
    }

    #[must_use]
    pub fn lobby(&self, scope: LobbyScope) -> Option<ReadyLobby> {
        self.lock()
            .scopes
            .get(&scope)
            .and_then(|scoped| scoped.lobby.clone())
    }

    #[must_use]
    pub fn persisted_lobby(&self, scope: LobbyScope) -> Option<PersistedLobbyState> {
        self.lobby(scope).map(|lobby| lobby.to_persisted())
    }

    pub fn restore_persisted_lobby(&self, state: PersistedLobbyState) {
        self.put_lobby(ReadyLobby::from_persisted(state));
    }

    pub fn join_lobby_at(&self, scope: LobbyScope, player_id: UserId, joined_at: f64) -> bool {
        let mut state = self.lock();
        let scoped = state.scopes.entry(scope).or_default();
        let lobby = scoped
            .lobby
            .get_or_insert_with(|| ReadyLobby::new(scope, player_id, "1970-01-01T00:00:00"));
        lobby.add_player_at(player_id, joined_at)
    }

    pub fn leave_lobby(&self, scope: LobbyScope, player_id: UserId) -> bool {
        self.lock()
            .scopes
            .get_mut(&scope)
            .and_then(|scoped| scoped.lobby.as_mut())
            .is_some_and(|lobby| lobby.remove_player(player_id))
    }

    pub fn reset_lobby(&self, scope: LobbyScope) {
        self.lock().scopes.remove(&scope);
    }

    pub fn set_readycheck_state(&self, scope: LobbyScope, input: ReadycheckStateInput) {
        let initial_reacted = input
            .initial_reacted
            .into_iter()
            .filter(|(player_id, _)| input.lobby_ids.contains(player_id))
            .collect();
        let generation = ReadycheckGeneration {
            message_id: input.message_id,
            channel_id: input.channel_id,
            lobby_ids: input.lobby_ids,
            player_data: input.player_data,
            reacted: initial_reacted,
            declined: BTreeSet::new(),
            created_at: input.created_at.unwrap_or_else(unix_time_now),
            completion_notified: false,
        };
        let mut state = self.lock();
        let scoped = state.scopes.entry(scope).or_default();
        scoped.readycheck = Some(generation);
        scoped.pending_publication = None;
    }

    #[must_use]
    pub fn readycheck_created_at(&self, scope: LobbyScope) -> Option<f64> {
        self.lock()
            .scopes
            .get(&scope)?
            .readycheck
            .as_ref()
            .map(|generation| generation.created_at)
    }

    #[must_use]
    pub fn confirmation_snapshot(&self, scope: LobbyScope) -> Option<ConfirmationSnapshot> {
        let state = self.lock();
        let generation = state.scopes.get(&scope)?.readycheck.as_ref()?;
        Some(ConfirmationSnapshot {
            message_id: generation.message_id,
            confirmed_ids: generation.reacted.keys().copied().collect(),
        })
    }

    #[must_use]
    pub fn readycheck_lobby_ids(&self, scope: LobbyScope) -> BTreeSet<UserId> {
        self.lock()
            .scopes
            .get(&scope)
            .and_then(|scoped| scoped.readycheck.as_ref())
            .map_or_else(BTreeSet::new, |generation| generation.lobby_ids.clone())
    }

    #[must_use]
    pub fn readycheck_reacted(&self, scope: LobbyScope) -> BTreeMap<UserId, String> {
        self.lock()
            .scopes
            .get(&scope)
            .and_then(|scoped| scoped.readycheck.as_ref())
            .map_or_else(BTreeMap::new, |generation| generation.reacted.clone())
    }

    #[must_use]
    pub fn readycheck_declined(&self, scope: LobbyScope) -> BTreeSet<UserId> {
        self.lock()
            .scopes
            .get(&scope)
            .and_then(|scoped| scoped.readycheck.as_ref())
            .map_or_else(BTreeSet::new, |generation| generation.declined.clone())
    }

    #[must_use]
    pub fn readycheck_player_data(
        &self,
        scope: LobbyScope,
    ) -> BTreeMap<UserId, ReadycheckPlayerData> {
        self.lock()
            .scopes
            .get(&scope)
            .and_then(|scoped| scoped.readycheck.as_ref())
            .map_or_else(BTreeMap::new, |generation| generation.player_data.clone())
    }

    #[must_use]
    pub fn readycheck_generation(&self, scope: LobbyScope) -> Option<ReadycheckGeneration> {
        self.lock()
            .scopes
            .get(&scope)
            .and_then(|scoped| scoped.readycheck.clone())
    }

    pub fn add_readycheck_reaction(
        &self,
        scope: LobbyScope,
        player_id: UserId,
        tag: impl Into<String>,
        expected_message_id: Option<MessageId>,
    ) -> bool {
        let mut state = self.lock();
        let Some(scoped) = state.scopes.get_mut(&scope) else {
            return false;
        };
        let live_members = scoped
            .lobby
            .as_ref()
            .filter(|lobby| lobby.status == LobbyStatus::Open)
            .map(|lobby| lobby.players.clone());
        let Some(generation) = scoped.readycheck.as_mut() else {
            return false;
        };
        if expected_message_id.is_some_and(|expected| expected != generation.message_id) {
            return false;
        }
        let eligible = live_members
            .as_ref()
            .unwrap_or(&generation.lobby_ids)
            .contains(&player_id);
        if !eligible || generation.reacted.contains_key(&player_id) {
            return false;
        }
        generation.reacted.insert(player_id, tag.into());
        generation.declined.remove(&player_id);
        true
    }

    pub fn remove_readycheck_reaction(
        &self,
        scope: LobbyScope,
        player_id: UserId,
        expected_message_id: Option<MessageId>,
    ) -> bool {
        let mut state = self.lock();
        let Some(generation) = state
            .scopes
            .get_mut(&scope)
            .and_then(|scoped| scoped.readycheck.as_mut())
        else {
            return false;
        };
        if expected_message_id.is_some_and(|expected| expected != generation.message_id)
            || generation.reacted.remove(&player_id).is_none()
        {
            return false;
        }
        generation.declined.insert(player_id);
        true
    }

    pub fn update_readycheck_data(
        &self,
        scope: LobbyScope,
        lobby_ids: BTreeSet<UserId>,
        player_data: BTreeMap<UserId, ReadycheckPlayerData>,
        expected_message_id: MessageId,
    ) -> Option<ReadycheckRosterUpdate> {
        let mut state = self.lock();
        let generation = state.scopes.get_mut(&scope)?.readycheck.as_mut()?;
        if generation.message_id != expected_message_id {
            return None;
        }
        let removed_confirmations = generation
            .reacted
            .keys()
            .copied()
            .filter(|player_id| !lobby_ids.contains(player_id))
            .collect::<BTreeSet<_>>();
        generation
            .reacted
            .retain(|player_id, _| lobby_ids.contains(player_id));
        generation
            .declined
            .retain(|player_id| lobby_ids.contains(player_id));
        generation.lobby_ids = lobby_ids;
        generation.player_data = player_data;
        Some(ReadycheckRosterUpdate {
            removed_confirmations,
        })
    }

    #[must_use]
    pub fn lobby_readycheck_snapshot(&self, scope: LobbyScope) -> Option<LobbyReadycheckSnapshot> {
        self.lobby_readycheck_snapshot_with(scope, || {})
    }

    fn lobby_readycheck_snapshot_with(
        &self,
        scope: LobbyScope,
        while_locked: impl FnOnce(),
    ) -> Option<LobbyReadycheckSnapshot> {
        let state = self.lock();
        while_locked();
        let scoped = state.scopes.get(&scope)?;
        let lobby = scoped
            .lobby
            .as_ref()
            .filter(|lobby| lobby.status == LobbyStatus::Open)?;
        let player_ids = lobby.players.iter().copied().collect::<Vec<_>>();
        let player_join_times = player_ids
            .iter()
            .filter_map(|player_id| {
                lobby
                    .player_join_times
                    .get(player_id)
                    .map(|timestamp| (*player_id, *timestamp))
            })
            .collect();
        let readycheck = scoped
            .readycheck
            .as_ref()
            .map(|generation| ConfirmationSnapshot {
                message_id: generation.message_id,
                confirmed_ids: generation.reacted.keys().copied().collect(),
            });
        Some(LobbyReadycheckSnapshot {
            player_ids,
            player_join_times,
            readycheck,
        })
    }

    #[cfg(test)]
    pub(crate) fn lobby_readycheck_snapshot_with_test_hook(
        &self,
        scope: LobbyScope,
        while_locked: impl FnOnce(),
    ) -> Option<LobbyReadycheckSnapshot> {
        self.lobby_readycheck_snapshot_with(scope, while_locked)
    }

    /// Atomically authorize and reserve/refresh a ready-check publication.
    /// Preconditions are evaluated before cooldown consumption.
    pub fn prepare_command(
        &self,
        request: ReadycheckCommandRequest,
        player_data: BTreeMap<UserId, ReadycheckPlayerData>,
    ) -> Result<ReadycheckCommandPlan, ReadycheckCommandFailure> {
        let Some(guild_id) = request.guild_id else {
            return Err(ReadycheckCommandFailure::NoGuild);
        };
        let scope = LobbyScope::new(guild_id, request.kind);
        let mut state = self.lock();

        let token = state.next_token.wrapping_add(1);
        // Token values are internal monotonic identities; consuming one on a
        // failed precondition is harmless and keeps the scoped mutable borrow
        // independent from the allocator field.
        state.next_token = token;
        let scoped = state
            .scopes
            .get_mut(&scope)
            .ok_or(ReadycheckCommandFailure::NoLobby)?;
        let lobby = scoped
            .lobby
            .as_mut()
            .filter(|lobby| lobby.status == LobbyStatus::Open)
            .ok_or(ReadycheckCommandFailure::NoLobby)?;
        if lobby.players.len() < request.minimum_players {
            return Err(ReadycheckCommandFailure::TooFewPlayers {
                minimum: request.minimum_players,
                current: lobby.players.len(),
            });
        }
        if !lobby.players.contains(&request.invoker_id) {
            return Err(ReadycheckCommandFailure::NotMember);
        }
        let thread_id = lobby.thread_id.ok_or(ReadycheckCommandFailure::NoThread)?;
        if scoped.pending_publication.is_some() {
            return Err(ReadycheckCommandFailure::PublicationInFlight);
        }
        if let Some(last_command_at) = scoped.last_command_at {
            let elapsed = request.now - last_command_at;
            if elapsed < READYCHECK_COOLDOWN_SECONDS {
                let remaining = (READYCHECK_COOLDOWN_SECONDS - elapsed).ceil().max(1.0) as u64;
                return Err(ReadycheckCommandFailure::Cooldown {
                    retry_after_seconds: remaining,
                });
            }
        }

        let current_message_id = scoped
            .readycheck
            .as_ref()
            .map(|generation| generation.message_id);
        let stale = scoped.readycheck.as_ref().is_some_and(|generation| {
            request.now - generation.created_at > READYCHECK_STALE_SECONDS
        });
        let can_refresh = scoped.readycheck.is_some()
            && !stale
            && request.existing_message == ExistingMessageState::Available;

        scoped.last_command_at = Some(request.now);
        if can_refresh {
            let generation = scoped.readycheck.as_mut().expect("checked above");
            let live_ids = lobby.players.clone();
            let departed_confirmations = generation
                .reacted
                .keys()
                .filter(|player_id| !live_ids.contains(player_id))
                .copied()
                .collect::<BTreeSet<_>>();
            generation.lobby_ids = live_ids.clone();
            generation.player_data = player_data;
            generation
                .reacted
                .retain(|player_id, _| live_ids.contains(player_id));
            generation
                .declined
                .retain(|player_id| live_ids.contains(player_id));
            if request.confirm_invoker {
                generation
                    .reacted
                    .entry(request.invoker_id)
                    .or_insert_with(|| format!("<@{}>", request.invoker_id.0));
                generation.declined.remove(&request.invoker_id);
            }
            let mention_ids = live_ids
                .difference(&generation.reacted.keys().copied().collect())
                .copied()
                .collect::<BTreeSet<_>>();
            let mut operations = departed_confirmations
                .into_iter()
                .map(
                    |player_id| ReadycheckTransportOperation::RemoveDepartedReaction {
                        channel_id: generation.channel_id,
                        message_id: generation.message_id,
                        player_id,
                        emoji: "✅",
                        requirement: DeliveryRequirement::BestEffort,
                    },
                )
                .collect::<Vec<_>>();
            operations.push(ReadycheckTransportOperation::EditReadycheck {
                channel_id: generation.channel_id,
                message_id: generation.message_id,
                requirement: DeliveryRequirement::Required,
            });
            return Ok(ReadycheckCommandPlan {
                scope,
                mode: PublicationMode::Refresh,
                permit: None,
                pruned_players: BTreeSet::new(),
                mention_ids,
                transport: ReadycheckTransportPlan { operations },
            });
        }

        let mut pruned_players = BTreeSet::new();
        if stale {
            let generation = scoped.readycheck.as_ref().expect("stale generation");
            let confirmed = generation.reacted.keys().copied().collect::<BTreeSet<_>>();
            for player_id in lobby.players.iter().copied().collect::<Vec<_>>() {
                let is_afk = player_data
                    .get(&player_id)
                    .is_some_and(|player| player.group == ReadinessGroup::Afk);
                let outside_grace = lobby
                    .player_join_times
                    .get(&player_id)
                    .is_none_or(|joined_at| request.now - joined_at >= RECENT_JOIN_GRACE_SECONDS);
                if is_afk
                    && outside_grace
                    && !confirmed.contains(&player_id)
                    && player_id != request.invoker_id
                    && !request.reserved_players.contains(&player_id)
                    && lobby.remove_player(player_id)
                {
                    pruned_players.insert(player_id);
                }
            }
        }

        let mode = if stale {
            PublicationMode::ReplaceStale
        } else if scoped.readycheck.is_some() {
            PublicationMode::RecoverMissing
        } else {
            PublicationMode::New
        };
        let expected_message_id = current_message_id;
        scoped.pending_publication = Some(PendingPublication {
            token,
            expected_message_id,
        });

        let live_ids = lobby.players.clone();
        let mention_ids: BTreeSet<UserId> = live_ids
            .iter()
            .copied()
            .filter(|player_id| {
                (!request.confirm_invoker || *player_id != request.invoker_id)
                    && player_data
                        .get(player_id)
                        .is_none_or(|data| data.group != ReadinessGroup::Recent)
            })
            .collect();
        let mut operations = Vec::new();
        if stale && let Some(generation) = scoped.readycheck.as_ref() {
            operations.push(ReadycheckTransportOperation::DeleteReadycheck {
                channel_id: generation.channel_id,
                message_id: generation.message_id,
                requirement: DeliveryRequirement::BestEffort,
            });
        }
        if !pruned_players.is_empty() {
            operations.push(ReadycheckTransportOperation::SyncLobbyDisplays {
                scope,
                requirement: DeliveryRequirement::Required,
            });
        }
        operations.push(ReadycheckTransportOperation::PostReadycheck {
            thread_id,
            requirement: DeliveryRequirement::Required,
        });
        operations.push(ReadycheckTransportOperation::ReactToPostedReadycheck {
            emoji: "✅",
            requirement: DeliveryRequirement::BestEffort,
        });
        if request.invocation_channel_id != thread_id && !mention_ids.is_empty() {
            operations.push(ReadycheckTransportOperation::MirrorPing {
                channel_id: request.invocation_channel_id,
                mentioned_users: mention_ids.clone(),
                requirement: DeliveryRequirement::Required,
            });
        }

        Ok(ReadycheckCommandPlan {
            scope,
            mode,
            permit: Some(ReadycheckPermit {
                token,
                scope,
                expected_message_id,
                invoker_id: request.invoker_id,
                confirm_invoker: request.confirm_invoker,
            }),
            pruned_players,
            mention_ids,
            transport: ReadycheckTransportPlan { operations },
        })
    }

    /// Commit a newly delivered Discord message only if the publication
    /// reservation and predecessor generation still match.
    pub fn commit_publication(
        &self,
        permit: ReadycheckPermit,
        message_id: MessageId,
        channel_id: ChannelId,
        player_data: BTreeMap<UserId, ReadycheckPlayerData>,
        created_at: f64,
    ) -> CommitPublicationResult {
        let mut state = self.lock();
        let Some(scoped) = state.scopes.get_mut(&permit.scope) else {
            return CommitPublicationResult::StalePermit;
        };
        let pending_matches = scoped.pending_publication.as_ref().is_some_and(|pending| {
            pending.token == permit.token
                && pending.expected_message_id == permit.expected_message_id
        });
        let current_matches = scoped
            .readycheck
            .as_ref()
            .map(|generation| generation.message_id)
            == permit.expected_message_id;
        if !pending_matches || !current_matches {
            return CommitPublicationResult::StalePermit;
        }
        let lobby_ids = scoped
            .lobby
            .as_ref()
            .filter(|lobby| lobby.status == LobbyStatus::Open)
            .map_or_else(BTreeSet::new, |lobby| lobby.players.clone());
        let mut reacted = BTreeMap::new();
        for (player_id, data) in &player_data {
            if data.group == ReadinessGroup::Recent && lobby_ids.contains(player_id) {
                reacted.insert(*player_id, format!("<@{}>", player_id.0));
            }
        }
        if permit.confirm_invoker && lobby_ids.contains(&permit.invoker_id) {
            reacted.insert(permit.invoker_id, format!("<@{}>", permit.invoker_id.0));
        }
        scoped.readycheck = Some(ReadycheckGeneration {
            message_id,
            channel_id,
            lobby_ids,
            player_data,
            reacted,
            declined: BTreeSet::new(),
            created_at,
            completion_notified: false,
        });
        scoped.pending_publication = None;
        CommitPublicationResult::Applied
    }

    pub fn abort_publication(&self, permit: &ReadycheckPermit) -> bool {
        let mut state = self.lock();
        let Some(scoped) = state.scopes.get_mut(&permit.scope) else {
            return false;
        };
        if scoped
            .pending_publication
            .as_ref()
            .is_none_or(|pending| pending.token != permit.token)
        {
            return false;
        }
        scoped.pending_publication = None;
        true
    }

    /// Exactly-once completion gate for one ready-check generation.
    #[must_use]
    pub fn take_completion_notification(
        &self,
        scope: LobbyScope,
        expected_message_id: MessageId,
    ) -> bool {
        let threshold = self
            .lock()
            .scopes
            .get(&scope)
            .and_then(|scoped| scoped.lobby.as_ref())
            .map_or(usize::MAX, |lobby| lobby.players.len());
        self.take_completion_notification_at_threshold(scope, expected_message_id, threshold)
    }

    /// Exactly-once completion gate for the configured matchmaking quorum.
    /// Extra lobby seats do not force every spectator/overflow player to
    /// confirm before `/shuffle` becomes available.
    #[must_use]
    pub fn take_completion_notification_at_threshold(
        &self,
        scope: LobbyScope,
        expected_message_id: MessageId,
        threshold: usize,
    ) -> bool {
        let mut state = self.lock();
        let Some(scoped) = state.scopes.get_mut(&scope) else {
            return false;
        };
        let live_members = scoped
            .lobby
            .as_ref()
            .filter(|lobby| lobby.status == LobbyStatus::Open)
            .map(|lobby| lobby.players.clone());
        let Some(generation) = scoped.readycheck.as_mut() else {
            return false;
        };
        let required = live_members.as_ref().unwrap_or(&generation.lobby_ids);
        let confirmed = required
            .iter()
            .filter(|player_id| generation.reacted.contains_key(player_id))
            .count();
        if generation.message_id != expected_message_id
            || generation.completion_notified
            || threshold == 0
            || confirmed < threshold
        {
            return false;
        }
        generation.completion_notified = true;
        true
    }

    /// Release a claimed completion delivery only while the same generation
    /// is still authoritative. Python marks a readycheck as announced only
    /// after Discord accepts the message, so transient delivery failures must
    /// remain retryable without reopening a replaced generation.
    pub fn release_completion_notification(
        &self,
        scope: LobbyScope,
        expected_message_id: MessageId,
    ) -> bool {
        let mut state = self.lock();
        let Some(generation) = state
            .scopes
            .get_mut(&scope)
            .and_then(|scoped| scoped.readycheck.as_mut())
        else {
            return false;
        };
        if generation.message_id != expected_message_id || !generation.completion_notified {
            return false;
        }
        generation.completion_notified = false;
        true
    }
}

fn unix_time_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

#[cfg(test)]
#[path = "readycheck/tests.rs"]
mod tests;
