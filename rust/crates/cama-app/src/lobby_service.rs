//! Discord-independent lobby orchestration.
//!
//! The live Python application still owns persistence and Discord callbacks.
//! This module ports the concurrency-sensitive lobby policy behind typed
//! player, pending-match, clock, and bonus-pool ports. All mutations and
//! ready-check snapshots for a guild/lobby kind share one mutex, matching the
//! canonical manager's atomic membership boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::moderation::LobbySuspension;
use cama_db::readycheck_repository::PersistedLobbyState;
use cama_domain::embed_safety::EmbedModel;
use thiserror::Error;

use crate::dedicated_lobby_channel::{
    ChannelId, GuildId, LobbyMessageIds, LobbyScope, MessageId, UserId,
};
use crate::embeds::{LobbyEmbedRequest, LobbyKind, LobbyPlayer, create_lobby_embed};

pub const LOWSKILL_RATING_CUTOFF: f64 = 1_400.0;
pub const DOTA_BET_SEED_AMOUNT: i64 = 50;

/// Existing-schema persistence boundary used by the production runtime.
///
/// The trait deliberately carries complete `lobby_state` rows so every
/// successful mutation preserves Python-owned routing metadata. Implementors
/// must not create or migrate schema.
pub trait LobbyPersistencePort: Send + Sync {
    fn load_all_lobbies(&self) -> Result<Vec<PersistedLobbyState>, String>;
    fn save_lobby(&self, lobby: &PersistedLobbyState) -> Result<(), String>;
    fn clear_lobby(&self, scope: LobbyScope) -> Result<bool, String>;
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LobbyPersistenceError {
    #[error("lobby persistence failed: {0}")]
    Storage(String),
}

pub trait LobbyPlayerPort {
    fn glicko_rating(&self, player_id: UserId, guild_id: GuildId) -> Option<f64>;

    fn get_by_ids(&self, player_ids: &[UserId], guild_id: GuildId) -> Vec<LobbyPlayer>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingMatchState {
    pub pending_match_id: i64,
    pub shuffle_message_jump_url: Option<String>,
}

pub trait PendingMatchPort {
    fn pending_match_for_player(
        &self,
        guild_id: GuildId,
        player_id: UserId,
    ) -> Option<PendingMatchState>;

    /// Fallible production read. In-memory ports retain the compact method
    /// above; durable adapters override this method so storage errors cannot
    /// silently admit a player already committed to a pending match.
    fn try_pending_match_for_player(
        &self,
        guild_id: GuildId,
        player_id: UserId,
    ) -> Result<Option<PendingMatchState>, String> {
        Ok(self.pending_match_for_player(guild_id, player_id))
    }
}

/// Existing-schema moderation boundary used by lobby membership mutations.
///
/// Reads may close a completed suspension, so callers must never hold the
/// lobby-state mutex while invoking this port.
pub trait LobbyModerationPort: Send + Sync {
    fn active_suspension(
        &self,
        player_id: UserId,
        scope: LobbyScope,
        now: i64,
    ) -> Result<Option<LobbySuspension>, String>;
}

pub trait LobbyClock {
    fn now_ns(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemLobbyClock;

impl LobbyClock for SystemLobbyClock {
    fn now_ns(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos().try_into().unwrap_or(i64::MAX))
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirstGamePoolPreviews {
    pub open: i64,
    pub low_skill: i64,
}

impl FirstGamePoolPreviews {
    #[must_use]
    pub const fn for_kind(self, kind: LobbyKind) -> i64 {
        match kind {
            LobbyKind::Open => self.open,
            LobbyKind::LowSkill => self.low_skill,
        }
    }
}

pub trait FirstGamePoolPreviewPort {
    fn first_game_pool_previews(
        &self,
        guild_id: GuildId,
        seed_amount: i64,
    ) -> FirstGamePoolPreviews;
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LobbyCreationError {
    #[error("only eligible players may create the low-skill lobby")]
    RatingTooHigh,
    #[error("player is suspended from this lobby")]
    Suspended(Box<LobbySuspension>),
    #[error("could not verify lobby suspension: {0}")]
    ModerationGuard(String),
    #[error(transparent)]
    Persistence(#[from] LobbyPersistenceError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JoinCommit {
    pub joined_at_ns: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JoinContext {
    Joined(JoinCommit),
    PendingMatch(PendingMatchState),
    Suspension(Box<LobbySuspension>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinFailure {
    InPendingMatch,
    LobbyFull,
    AlreadyJoined,
    RatingTooHigh,
    InFlight,
    Suspended,
    Persistence,
    PendingMatchGuard,
    ModerationGuard,
    ConditionalJoinDeprecated,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JoinOutcome {
    pub success: bool,
    pub failure: Option<JoinFailure>,
    pub context: Option<JoinContext>,
}

impl JoinOutcome {
    fn joined(joined_at_ns: i64) -> Self {
        Self {
            success: true,
            failure: None,
            context: Some(JoinContext::Joined(JoinCommit { joined_at_ns })),
        }
    }

    fn failed(failure: JoinFailure) -> Self {
        Self {
            success: false,
            failure: Some(failure),
            context: None,
        }
    }

    fn pending(state: PendingMatchState) -> Self {
        Self {
            success: false,
            failure: Some(JoinFailure::InPendingMatch),
            context: Some(JoinContext::PendingMatch(state)),
        }
    }

    fn suspended(state: LobbySuspension) -> Self {
        Self {
            success: false,
            failure: Some(JoinFailure::Suspended),
            context: Some(JoinContext::Suspension(Box::new(state))),
        }
    }

    #[must_use]
    pub const fn join_commit(&self) -> Option<JoinCommit> {
        match self.context {
            Some(JoinContext::Joined(commit)) => Some(commit),
            Some(JoinContext::PendingMatch(_) | JoinContext::Suspension(_)) | None => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LobbySnapshot {
    pub scope: LobbyScope,
    pub created_by: Option<UserId>,
    pub created_at_ns: i64,
    pub players: BTreeSet<UserId>,
    pub player_join_times: BTreeMap<UserId, f64>,
    pub message_ids: LobbyMessageIds,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadycheckConfirmationSnapshot {
    pub message_id: MessageId,
    pub reacted: BTreeSet<UserId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedLobbyReadycheckSnapshot {
    pub player_ids: Vec<UserId>,
    pub players: Vec<LobbyPlayer>,
    pub player_join_times: BTreeMap<UserId, f64>,
    pub readycheck: Option<ReadycheckConfirmationSnapshot>,
}

#[derive(Clone, Debug, Default)]
struct ReadycheckRecord {
    message_id: Option<MessageId>,
    channel_id: Option<ChannelId>,
    lobby_ids: BTreeSet<UserId>,
    reacted: BTreeMap<UserId, String>,
}

impl ReadycheckRecord {
    fn confirmation_snapshot(&self) -> Option<ReadycheckConfirmationSnapshot> {
        Some(ReadycheckConfirmationSnapshot {
            message_id: self.message_id?,
            reacted: self.reacted.keys().copied().collect(),
        })
    }
}

#[derive(Clone, Debug)]
struct LobbyRecord {
    scope: LobbyScope,
    created_by: Option<UserId>,
    created_at_ns: i64,
    players: BTreeSet<UserId>,
    player_join_times: BTreeMap<UserId, f64>,
    message_ids: LobbyMessageIds,
    readycheck: ReadycheckRecord,
}

impl LobbyRecord {
    fn new(scope: LobbyScope, created_by: Option<UserId>, created_at_ns: i64) -> Self {
        Self {
            scope,
            created_by,
            created_at_ns,
            players: BTreeSet::new(),
            player_join_times: BTreeMap::new(),
            message_ids: LobbyMessageIds::default(),
            readycheck: ReadycheckRecord::default(),
        }
    }

    fn snapshot(&self) -> LobbySnapshot {
        LobbySnapshot {
            scope: self.scope,
            created_by: self.created_by,
            created_at_ns: self.created_at_ns,
            players: self.players.clone(),
            player_join_times: self.player_join_times.clone(),
            message_ids: self.message_ids.clone(),
        }
    }

    fn from_persisted(state: PersistedLobbyState) -> Option<Self> {
        if !state.status.eq_ignore_ascii_case("open") {
            return None;
        }
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
        let created_at_ns = chrono::DateTime::parse_from_rfc3339(&state.created_at)
            .ok()
            .and_then(|created_at| created_at.timestamp_nanos_opt())
            .unwrap_or_default();
        Some(Self {
            scope: LobbyScope::new(GuildId(state.guild_id), kind),
            created_by: Some(UserId(state.created_by)),
            created_at_ns,
            player_join_times: state
                .player_join_times
                .into_iter()
                .filter_map(|(player_id, joined_at)| {
                    players
                        .contains(&UserId(player_id))
                        .then_some((UserId(player_id), joined_at))
                })
                .collect(),
            players,
            message_ids: LobbyMessageIds {
                message_id: state.message_id.map(MessageId),
                channel_id: state.channel_id.map(ChannelId),
                thread_id: state.thread_id.map(ChannelId),
                embed_message_id: state.embed_message_id.map(MessageId),
                origin_channel_id: state.origin_channel_id.map(ChannelId),
            },
            readycheck: ReadycheckRecord::default(),
        })
    }

    fn to_persisted(&self) -> PersistedLobbyState {
        let created_at = chrono::DateTime::from_timestamp_nanos(self.created_at_ns).to_rfc3339();
        PersistedLobbyState {
            lobby_id: self.scope.lobby_id(),
            guild_id: self.scope.guild_id.0,
            players: self.players.iter().map(|player_id| player_id.0).collect(),
            conditional_players: Vec::new(),
            status: "open".to_owned(),
            created_by: self.created_by.unwrap_or(UserId(0)).0,
            created_at,
            message_id: self.message_ids.message_id.map(|message_id| message_id.0),
            channel_id: self.message_ids.channel_id.map(|channel_id| channel_id.0),
            thread_id: self.message_ids.thread_id.map(|channel_id| channel_id.0),
            embed_message_id: self
                .message_ids
                .embed_message_id
                .map(|message_id| message_id.0),
            origin_channel_id: self
                .message_ids
                .origin_channel_id
                .map(|channel_id| channel_id.0),
            player_join_times: self
                .player_join_times
                .iter()
                .map(|(player_id, joined_at)| (player_id.0, *joined_at))
                .collect(),
        }
    }
}

#[derive(Debug, Default)]
struct LobbyState {
    lobbies: BTreeMap<LobbyScope, LobbyRecord>,
    in_flight: BTreeMap<(GuildId, UserId), LobbyKind>,
}

/// In-memory policy port for the Python `LobbyManagerService` contract.
///
/// Persistence is intentionally not hidden here: the production composition
/// must load/save `lobby_state` around these operations until a dedicated Rust
/// runtime adapter is cut over.
pub struct LobbyService<P, M, C> {
    player_port: P,
    pending_match_port: Option<M>,
    clock: C,
    ready_threshold: usize,
    max_players: usize,
    persistence: Option<Arc<dyn LobbyPersistencePort>>,
    moderation: Option<Arc<dyn LobbyModerationPort>>,
    state: Mutex<LobbyState>,
}

impl<P, M, C> LobbyService<P, M, C>
where
    P: LobbyPlayerPort,
    M: PendingMatchPort,
    C: LobbyClock,
{
    #[must_use]
    pub fn new(
        player_port: P,
        pending_match_port: Option<M>,
        clock: C,
        ready_threshold: usize,
        max_players: usize,
    ) -> Self {
        Self {
            player_port,
            pending_match_port,
            clock,
            ready_threshold,
            max_players,
            persistence: None,
            moderation: None,
            state: Mutex::new(LobbyState::default()),
        }
    }

    /// Attach the authoritative guild-scoped suspension policy.
    #[must_use]
    pub fn with_moderation(mut self, moderation: Arc<dyn LobbyModerationPort>) -> Self {
        self.moderation = Some(moderation);
        self
    }

    /// Attach the existing `lobby_state` store and hydrate every open scope.
    /// The service is not published to callers until hydration succeeds.
    pub fn with_persistence(
        mut self,
        persistence: Arc<dyn LobbyPersistencePort>,
    ) -> Result<Self, LobbyPersistenceError> {
        let rows = persistence
            .load_all_lobbies()
            .map_err(LobbyPersistenceError::Storage)?;
        let lobbies = rows
            .into_iter()
            .filter_map(LobbyRecord::from_persisted)
            .map(|lobby| (lobby.scope, lobby))
            .collect();
        self.persistence = Some(persistence);
        self.state = Mutex::new(LobbyState {
            lobbies,
            in_flight: BTreeMap::new(),
        });
        Ok(self)
    }

    fn lock_state(&self) -> MutexGuard<'_, LobbyState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn is_low_skill_eligible(&self, player_id: UserId, guild_id: GuildId) -> bool {
        self.player_port
            .glicko_rating(player_id, guild_id)
            .is_some_and(|rating| rating < LOWSKILL_RATING_CUTOFF)
    }

    fn persist_record(&self, record: &LobbyRecord) -> Result<(), LobbyPersistenceError> {
        self.persistence.as_ref().map_or(Ok(()), |persistence| {
            persistence
                .save_lobby(&record.to_persisted())
                .map_err(LobbyPersistenceError::Storage)
        })
    }

    fn active_suspension(
        &self,
        player_id: UserId,
        scope: LobbyScope,
    ) -> Result<Option<LobbySuspension>, String> {
        self.moderation.as_ref().map_or(Ok(None), |moderation| {
            moderation.active_suspension(
                player_id,
                scope,
                self.clock.now_ns().saturating_div(1_000_000_000),
            )
        })
    }

    fn rollback_provisional_join(
        &self,
        state: &mut LobbyState,
        scope: LobbyScope,
        player_id: UserId,
        created_shell: bool,
    ) {
        let Some(mut candidate) = state.lobbies.get(&scope).cloned() else {
            return;
        };
        candidate.players.remove(&player_id);
        candidate.player_join_times.remove(&player_id);
        if created_shell && candidate.players.is_empty() {
            let cleared = self
                .persistence
                .as_ref()
                .map_or(Ok(false), |persistence| persistence.clear_lobby(scope));
            if cleared.is_ok() {
                state.lobbies.remove(&scope);
                return;
            }
        }
        // Preserve the in-memory rollback even if cleanup persistence is
        // temporarily unavailable. A later mutation/reconciliation can retry.
        let _ = self.persist_record(&candidate);
        state.lobbies.insert(scope, candidate);
    }

    pub fn get_or_create_lobby(
        &self,
        creator_id: Option<UserId>,
        scope: LobbyScope,
    ) -> Result<LobbySnapshot, LobbyCreationError> {
        {
            let state = self.lock_state();
            if let Some(lobby) = state.lobbies.get(&scope) {
                return Ok(lobby.snapshot());
            }
        }
        if scope.kind == LobbyKind::LowSkill
            && creator_id
                .is_some_and(|player_id| !self.is_low_skill_eligible(player_id, scope.guild_id))
        {
            return Err(LobbyCreationError::RatingTooHigh);
        }
        if let Some(creator_id) = creator_id
            && let Some(suspension) = self
                .active_suspension(creator_id, scope)
                .map_err(LobbyCreationError::ModerationGuard)?
        {
            return Err(LobbyCreationError::Suspended(Box::new(suspension)));
        }
        let mut state = self.lock_state();
        // A concurrent creator may have published this scope while the
        // moderation read ran. Existing lobbies remain viewable to suspended
        // users, matching Python's get-or-create contract.
        if let Some(lobby) = state.lobbies.get(&scope) {
            return Ok(lobby.snapshot());
        }
        let record = LobbyRecord::new(scope, creator_id, self.clock.now_ns());
        let snapshot = record.snapshot();
        self.persist_record(&record)?;
        state.lobbies.insert(scope, record);
        Ok(snapshot)
    }

    #[must_use]
    pub fn get_lobby(&self, scope: LobbyScope) -> Option<LobbySnapshot> {
        self.lock_state()
            .lobbies
            .get(&scope)
            .map(LobbyRecord::snapshot)
    }

    /// Stable copy used while READY/resume reconciliation performs Discord
    /// I/O without holding the lobby-state mutex.
    #[must_use]
    pub fn open_lobbies(&self) -> Vec<LobbySnapshot> {
        self.lock_state()
            .lobbies
            .values()
            .map(LobbyRecord::snapshot)
            .collect()
    }

    #[must_use]
    pub fn get_lobby_kinds_for_player(
        &self,
        player_id: UserId,
        guild_id: GuildId,
    ) -> Vec<LobbyKind> {
        let state = self.lock_state();
        [LobbyKind::Open, LobbyKind::LowSkill]
            .into_iter()
            .filter(|kind| {
                state
                    .lobbies
                    .get(&LobbyScope::new(guild_id, *kind))
                    .is_some_and(|lobby| lobby.players.contains(&player_id))
            })
            .collect()
    }

    #[must_use]
    pub fn join_lobby(&self, player_id: UserId, scope: LobbyScope) -> JoinOutcome {
        if let Some(port) = self.pending_match_port.as_ref() {
            match port.try_pending_match_for_player(scope.guild_id, player_id) {
                Ok(Some(pending)) => return JoinOutcome::pending(pending),
                Ok(None) => {}
                Err(_) => return JoinOutcome::failed(JoinFailure::PendingMatchGuard),
            }
        }
        if scope.kind == LobbyKind::LowSkill
            && !self.is_low_skill_eligible(player_id, scope.guild_id)
        {
            return JoinOutcome::failed(JoinFailure::RatingTooHigh);
        }

        {
            let state = self.lock_state();
            if state
                .in_flight
                .get(&(scope.guild_id, player_id))
                .is_some_and(|reserved_kind| *reserved_kind != scope.kind)
            {
                return JoinOutcome::failed(JoinFailure::InFlight);
            }
        }

        match self.active_suspension(player_id, scope) {
            Ok(Some(suspension)) => return JoinOutcome::suspended(suspension),
            Ok(None) => {}
            Err(_) => return JoinOutcome::failed(JoinFailure::ModerationGuard),
        }

        let joined_at_ns = self.clock.now_ns();
        let created_shell;
        {
            let mut state = self.lock_state();
            // Re-verify cheap invariants after the unlocked moderation lookup.
            if state
                .in_flight
                .get(&(scope.guild_id, player_id))
                .is_some_and(|reserved_kind| *reserved_kind != scope.kind)
            {
                return JoinOutcome::failed(JoinFailure::InFlight);
            }
            created_shell = !state.lobbies.contains_key(&scope);
            let candidate = state
                .lobbies
                .entry(scope)
                .or_insert_with(|| LobbyRecord::new(scope, None, self.clock.now_ns()));
            if candidate.players.len() >= self.max_players {
                return JoinOutcome::failed(JoinFailure::LobbyFull);
            }
            if !candidate.players.insert(player_id) {
                return JoinOutcome::failed(JoinFailure::AlreadyJoined);
            }
            #[allow(clippy::cast_precision_loss)]
            candidate
                .player_join_times
                .insert(player_id, joined_at_ns as f64 / 1_000_000_000.0);
        }

        // Close the unlocked moderation TOCTOU window after the provisional
        // in-memory add but before its first durable write. Retry one locked-DB
        // blip, then fail open exactly like Python so the caller never sees a
        // failed command while the provisional member remains seated.
        let post_add_suspension = match self.active_suspension(player_id, scope) {
            Ok(suspension) => suspension,
            Err(_) => self.active_suspension(player_id, scope).unwrap_or(None),
        };
        let mut state = self.lock_state();
        if let Some(suspension) = post_add_suspension {
            let reserved_by_target =
                state.in_flight.get(&(scope.guild_id, player_id)) == Some(&scope.kind);
            if !reserved_by_target {
                self.rollback_provisional_join(&mut state, scope, player_id, created_shell);
                return JoinOutcome::suspended(suspension);
            }
        }
        let Some(candidate) = state.lobbies.get(&scope).cloned() else {
            return JoinOutcome::failed(JoinFailure::Persistence);
        };
        if self.persist_record(&candidate).is_err() {
            self.rollback_provisional_join(&mut state, scope, player_id, created_shell);
            return JoinOutcome::failed(JoinFailure::Persistence);
        }
        JoinOutcome::joined(joined_at_ns)
    }

    #[must_use]
    pub const fn join_lobby_conditional(&self) -> JoinOutcome {
        JoinOutcome {
            success: false,
            failure: Some(JoinFailure::ConditionalJoinDeprecated),
            context: None,
        }
    }

    #[must_use]
    pub fn get_in_flight_lobby_kind_for_player(
        &self,
        player_id: UserId,
        guild_id: GuildId,
    ) -> Option<LobbyKind> {
        self.lock_state()
            .in_flight
            .get(&(guild_id, player_id))
            .copied()
    }

    pub fn reserve_lobby_players(&self, player_ids: &BTreeSet<UserId>, scope: LobbyScope) -> bool {
        if player_ids
            .iter()
            .any(|player_id| !matches!(self.active_suspension(*player_id, scope), Ok(None)))
        {
            return false;
        }
        let mut state = self.lock_state();
        let can_reserve = state
            .lobbies
            .get(&scope)
            .is_some_and(|lobby| player_ids.is_subset(&lobby.players))
            && player_ids
                .iter()
                .all(|player_id| !state.in_flight.contains_key(&(scope.guild_id, *player_id)));
        if !can_reserve {
            return false;
        }
        for player_id in player_ids {
            state
                .in_flight
                .insert((scope.guild_id, *player_id), scope.kind);
        }
        true
    }

    pub fn release_lobby_players(&self, player_ids: &BTreeSet<UserId>, scope: LobbyScope) {
        let mut state = self.lock_state();
        for player_id in player_ids {
            let key = (scope.guild_id, *player_id);
            if state.in_flight.get(&key) == Some(&scope.kind) {
                state.in_flight.remove(&key);
            }
        }
    }

    #[must_use]
    pub fn has_in_flight_lobby_players(&self, scope: LobbyScope) -> bool {
        self.lock_state()
            .in_flight
            .iter()
            .any(|((guild_id, _), kind)| *guild_id == scope.guild_id && *kind == scope.kind)
    }

    pub fn leave_lobby(&self, player_id: UserId, scope: LobbyScope) -> bool {
        self.try_leave_lobby(player_id, scope).unwrap_or(false)
    }

    pub fn try_leave_lobby(
        &self,
        player_id: UserId,
        scope: LobbyScope,
    ) -> Result<bool, LobbyPersistenceError> {
        let mut state = self.lock_state();
        if state.in_flight.get(&(scope.guild_id, player_id)) == Some(&scope.kind) {
            return Ok(false);
        }
        let Some(mut candidate) = state.lobbies.get(&scope).cloned() else {
            return Ok(false);
        };
        let removed = candidate.players.remove(&player_id);
        if removed {
            candidate.player_join_times.remove(&player_id);
            self.persist_record(&candidate)?;
            state.lobbies.insert(scope, candidate);
        }
        Ok(removed)
    }

    /// Admin test support: replace seated players' durable join timestamps in
    /// one lobby-state commit. The caller serializes this with the same scope
    /// operation lock used by joins/leaves; persistence is committed before
    /// the in-memory snapshot.
    pub fn try_set_player_join_times(
        &self,
        scope: LobbyScope,
        joined_at_seconds: &BTreeMap<UserId, f64>,
    ) -> Result<usize, LobbyPersistenceError> {
        let mut state = self.lock_state();
        let Some(mut candidate) = state.lobbies.get(&scope).cloned() else {
            return Ok(0);
        };
        let mut updated = 0;
        for (player_id, joined_at_seconds) in joined_at_seconds {
            if candidate.players.contains(player_id) {
                candidate
                    .player_join_times
                    .insert(*player_id, *joined_at_seconds);
                updated += 1;
            }
        }
        if updated == 0 {
            return Ok(0);
        }
        self.persist_record(&candidate)?;
        state.lobbies.insert(scope, candidate);
        Ok(updated)
    }

    pub fn remove_players_from_lobby(
        &self,
        player_ids: &BTreeSet<UserId>,
        scope: LobbyScope,
    ) -> BTreeSet<UserId> {
        self.try_remove_players_from_lobby(player_ids, scope)
            .unwrap_or_default()
    }

    pub fn try_remove_players_from_lobby(
        &self,
        player_ids: &BTreeSet<UserId>,
        scope: LobbyScope,
    ) -> Result<BTreeSet<UserId>, LobbyPersistenceError> {
        let mut state = self.lock_state();
        let pinned = player_ids
            .iter()
            .filter(|player_id| {
                state.in_flight.get(&(scope.guild_id, **player_id)) == Some(&scope.kind)
            })
            .copied()
            .collect::<BTreeSet<_>>();
        let Some(mut candidate) = state.lobbies.get(&scope).cloned() else {
            return Ok(BTreeSet::new());
        };
        let removed = player_ids
            .iter()
            .filter(|player_id| !pinned.contains(player_id) && candidate.players.remove(player_id))
            .copied()
            .collect::<BTreeSet<_>>();
        for player_id in &removed {
            candidate.player_join_times.remove(player_id);
        }
        if !removed.is_empty() {
            self.persist_record(&candidate)?;
            state.lobbies.insert(scope, candidate);
        }
        Ok(removed)
    }

    /// Reconcile one ready-check generation with the lobby's current roster.
    ///
    /// Cross-lobby match eviction mutates membership before any best-effort
    /// Discord repaint.  Keeping this reconciliation inside the same typed
    /// service means a failed display edit cannot leave departed players
    /// counted toward the ready quorum.
    pub fn prune_readycheck_to_lobby_members(&self, scope: LobbyScope) -> usize {
        let mut state = self.lock_state();
        let Some(lobby) = state.lobbies.get_mut(&scope) else {
            return 0;
        };
        let players = lobby.players.clone();
        lobby
            .readycheck
            .lobby_ids
            .retain(|player_id| players.contains(player_id));
        lobby
            .readycheck
            .reacted
            .retain(|player_id, _| players.contains(player_id));
        lobby.readycheck.reacted.len()
    }

    #[must_use]
    pub fn readycheck_confirmed_count(&self, scope: LobbyScope) -> usize {
        self.lock_state()
            .lobbies
            .get(&scope)
            .map_or(0, |lobby| lobby.readycheck.reacted.len())
    }

    #[must_use]
    pub const fn ready_threshold(&self) -> usize {
        self.ready_threshold
    }

    pub fn set_lobby_message_ids(&self, scope: LobbyScope, ids: LobbyMessageIds) -> bool {
        self.try_set_lobby_message_ids(scope, ids).unwrap_or(false)
    }

    pub fn try_set_lobby_message_ids(
        &self,
        scope: LobbyScope,
        ids: LobbyMessageIds,
    ) -> Result<bool, LobbyPersistenceError> {
        let mut state = self.lock_state();
        let Some(mut candidate) = state.lobbies.get(&scope).cloned() else {
            return Ok(false);
        };
        candidate.message_ids = ids;
        self.persist_record(&candidate)?;
        state.lobbies.insert(scope, candidate);
        Ok(true)
    }

    #[must_use]
    pub fn get_lobby_message_id(&self, scope: LobbyScope) -> Option<MessageId> {
        self.lock_state()
            .lobbies
            .get(&scope)
            .and_then(|lobby| lobby.message_ids.message_id)
    }

    pub fn set_readycheck_state(
        &self,
        scope: LobbyScope,
        message_id: MessageId,
        channel_id: ChannelId,
        lobby_ids: BTreeSet<UserId>,
        initial_reacted: BTreeMap<UserId, String>,
    ) -> bool {
        let mut state = self.lock_state();
        let Some(lobby) = state.lobbies.get_mut(&scope) else {
            return false;
        };
        lobby.readycheck = ReadycheckRecord {
            message_id: Some(message_id),
            channel_id: Some(channel_id),
            lobby_ids,
            reacted: initial_reacted,
        };
        true
    }

    #[must_use]
    pub fn get_readycheck_message_id(&self, scope: LobbyScope) -> Option<MessageId> {
        self.lock_state()
            .lobbies
            .get(&scope)
            .and_then(|lobby| lobby.readycheck.message_id)
    }

    #[must_use]
    pub fn get_readycheck_channel_id(&self, scope: LobbyScope) -> Option<ChannelId> {
        self.lock_state()
            .lobbies
            .get(&scope)
            .and_then(|lobby| lobby.readycheck.channel_id)
    }

    pub fn add_readycheck_reaction(
        &self,
        scope: LobbyScope,
        player_id: UserId,
        tag: impl Into<String>,
    ) -> bool {
        let mut state = self.lock_state();
        let Some(lobby) = state.lobbies.get_mut(&scope) else {
            return false;
        };
        if lobby.readycheck.message_id.is_none() || !lobby.readycheck.lobby_ids.contains(&player_id)
        {
            return false;
        }
        lobby
            .readycheck
            .reacted
            .insert(player_id, tag.into())
            .is_none()
    }

    #[must_use]
    pub fn get_readycheck_confirmation_snapshot(
        &self,
        scope: LobbyScope,
    ) -> Option<ReadycheckConfirmationSnapshot> {
        self.lock_state()
            .lobbies
            .get(&scope)
            .and_then(|lobby| lobby.readycheck.confirmation_snapshot())
    }

    #[must_use]
    pub fn get_lobby_players_and_readycheck_snapshot(
        &self,
        scope: LobbyScope,
    ) -> Option<LoadedLobbyReadycheckSnapshot> {
        let (player_ids, player_join_times, readycheck) = {
            let state = self.lock_state();
            let lobby = state.lobbies.get(&scope)?;
            (
                lobby.players.iter().copied().collect::<Vec<_>>(),
                lobby.player_join_times.clone(),
                lobby.readycheck.confirmation_snapshot(),
            )
        };
        let players = self.player_port.get_by_ids(&player_ids, scope.guild_id);
        Some(LoadedLobbyReadycheckSnapshot {
            player_ids,
            players,
            player_join_times,
            readycheck,
        })
    }

    pub fn reset_lobby(&self, scope: LobbyScope) -> bool {
        self.try_reset_lobby(scope).unwrap_or(false)
    }

    pub fn try_reset_lobby(&self, scope: LobbyScope) -> Result<bool, LobbyPersistenceError> {
        let mut state = self.lock_state();
        if !state.lobbies.contains_key(&scope) {
            return Ok(false);
        }
        if let Some(persistence) = &self.persistence {
            persistence
                .clear_lobby(scope)
                .map_err(LobbyPersistenceError::Storage)?;
        }
        let removed = state.lobbies.remove(&scope).is_some();
        state
            .in_flight
            .retain(|(guild_id, _), kind| *guild_id != scope.guild_id || *kind != scope.kind);
        Ok(removed)
    }

    #[must_use]
    pub fn build_lobby_embed(
        &self,
        lobby: &LobbySnapshot,
        preview_port: Option<&dyn FirstGamePoolPreviewPort>,
    ) -> EmbedModel {
        let player_ids = lobby.players.iter().copied().collect::<Vec<_>>();
        let players = self
            .player_port
            .get_by_ids(&player_ids, lobby.scope.guild_id);
        let bonus_pool_preview = preview_port.map(|port| {
            port.first_game_pool_previews(lobby.scope.guild_id, DOTA_BET_SEED_AMOUNT)
                .for_kind(lobby.scope.kind)
        });
        create_lobby_embed(
            LobbyEmbedRequest {
                kind: lobby.scope.kind,
                created_at_epoch: Some(lobby.created_at_ns / 1_000_000_000),
                regular_count: lobby.players.len(),
                ready_threshold: self.ready_threshold,
                max_players: self.max_players,
                guild_id: Some(lobby.scope.guild_id.0),
                bonus_pool_preview,
            },
            &players,
            &player_ids
                .iter()
                .map(|player_id| player_id.0)
                .collect::<Vec<_>>(),
            None,
        )
    }
}

#[cfg(test)]
#[path = "lobby_service/tests.rs"]
mod tests;
