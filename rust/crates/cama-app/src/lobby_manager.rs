//! Persistent, Discord-independent lobby lifecycle orchestration.
//!
//! Python remains the live runtime and migration authority during the
//! side-by-side rewrite. This module consumes the existing `lobby_state`
//! repository port, bulk-rehydrates every guild at startup, and immediately
//! writes each successful lifecycle mutation back through that port.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::readycheck_repository::{PersistedLobbyState, ReadycheckLobbyStore};

use crate::dedicated_lobby_channel::{
    ChannelId, GuildId, LobbyMessageIds, LobbyScope, MessageId, UserId,
};
use crate::embeds::LobbyKind;

pub const READY_PLAYER_COUNT: usize = 10;
pub const MAX_LOBBY_PLAYERS: usize = 12;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LobbyStatus {
    Open,
    Closed,
    Other(String),
}

impl LobbyStatus {
    fn from_persisted(status: String) -> Self {
        match status.as_str() {
            "open" => Self::Open,
            "closed" => Self::Closed,
            _ => Self::Other(status),
        }
    }

    fn as_persisted(&self) -> &str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Other(status) => status,
        }
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LobbyRole {
    Carry,
    Mid,
    Offlane,
    SoftSupport,
    HardSupport,
}

impl LobbyRole {
    const fn index(self) -> usize {
        match self {
            Self::Carry => 0,
            Self::Mid => 1,
            Self::Offlane => 2,
            Self::SoftSupport => 3,
            Self::HardSupport => 4,
        }
    }
}

/// One lobby with invariants that are independent of persistence and Discord.
#[derive(Clone, Debug, PartialEq)]
pub struct LobbyModel {
    pub scope: LobbyScope,
    pub created_by: UserId,
    pub created_at: String,
    pub players: BTreeSet<UserId>,
    pub player_join_times: BTreeMap<UserId, f64>,
    pub status: LobbyStatus,
    pub message_ids: LobbyMessageIds,
}

impl LobbyModel {
    #[must_use]
    pub fn open(scope: LobbyScope, created_by: UserId, created_at: impl Into<String>) -> Self {
        Self {
            scope,
            created_by,
            created_at: created_at.into(),
            players: BTreeSet::new(),
            player_join_times: BTreeMap::new(),
            status: LobbyStatus::Open,
            message_ids: LobbyMessageIds::default(),
        }
    }

    #[must_use]
    pub fn from_persisted(mut state: PersistedLobbyState) -> Self {
        state.sanitize_legacy_conditionals();
        let kind = if state.lobby_id
            == LobbyScope::new(GuildId::DEFAULT, LobbyKind::LowSkill).lobby_id()
        {
            LobbyKind::LowSkill
        } else {
            LobbyKind::Open
        };
        let players = state.players.into_iter().map(UserId).collect();
        let player_join_times = state
            .player_join_times
            .into_iter()
            .map(|(player_id, joined_at)| (UserId(player_id), joined_at))
            .collect();

        Self {
            scope: LobbyScope::new(GuildId(state.guild_id), kind),
            created_by: UserId(state.created_by),
            created_at: state.created_at,
            players,
            player_join_times,
            status: LobbyStatus::from_persisted(state.status),
            message_ids: LobbyMessageIds {
                message_id: state.message_id.map(MessageId),
                channel_id: state.channel_id.map(ChannelId),
                thread_id: state.thread_id.map(ChannelId),
                embed_message_id: state.embed_message_id.map(MessageId),
                origin_channel_id: state.origin_channel_id.map(ChannelId),
            },
        }
    }

    #[must_use]
    pub fn to_persisted(&self) -> PersistedLobbyState {
        PersistedLobbyState {
            lobby_id: self.scope.lobby_id(),
            guild_id: self.scope.guild_id.0,
            players: self.players.iter().map(|player_id| player_id.0).collect(),
            conditional_players: Vec::new(),
            status: self.status.as_persisted().to_owned(),
            created_by: self.created_by.0,
            created_at: self.created_at.clone(),
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

    #[must_use]
    pub fn add_player(&mut self, player_id: UserId, joined_at: f64) -> bool {
        if !self.status.is_open() || !self.players.insert(player_id) {
            return false;
        }
        self.player_join_times.entry(player_id).or_insert(joined_at);
        true
    }

    #[must_use]
    pub fn remove_player(&mut self, player_id: UserId) -> bool {
        if !self.players.remove(&player_id) {
            return false;
        }
        self.player_join_times.remove(&player_id);
        true
    }

    pub fn close(&mut self) {
        self.status = LobbyStatus::Closed;
    }

    #[must_use]
    pub const fn conditional_player_count(&self) -> usize {
        0
    }

    #[must_use]
    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    #[must_use]
    pub fn total_count(&self) -> usize {
        self.players.len()
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.is_ready_with_minimum(READY_PLAYER_COUNT)
    }

    #[must_use]
    pub fn is_ready_with_minimum(&self, minimum: usize) -> bool {
        self.total_count() >= minimum
    }

    /// Match Python's primary-role check: every Dota role needs two players.
    #[must_use]
    pub fn can_create_teams(&self, roles: &BTreeMap<UserId, Vec<LobbyRole>>) -> bool {
        let mut counts = [0_usize; 5];
        for player_id in &self.players {
            if let Some(primary_role) = roles.get(player_id).and_then(|roles| roles.first()) {
                counts[primary_role.index()] += 1;
            }
        }
        counts.into_iter().all(|count| count >= 2)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LobbyJoinOutcome {
    Ok,
    Full,
    AlreadyJoined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConditionalLobbyJoinOutcome {
    Deprecated,
}

/// Generic write-through manager over the existing Python-compatible store.
pub struct PersistentLobbyManager<S> {
    store: S,
    lobbies: BTreeMap<LobbyScope, LobbyModel>,
}

impl<S> PersistentLobbyManager<S>
where
    S: ReadycheckLobbyStore,
{
    /// Hydrate solely from the repository's bulk path. Legacy conditional
    /// players and their orphan timestamps are sanitized and written back.
    pub fn new(store: S) -> Result<Self, S::Error> {
        let rows = store.load_all_lobby_states()?;
        let mut lobbies = BTreeMap::new();
        for mut row in rows {
            if row.sanitize_legacy_conditionals() {
                store.save_lobby_state(&row)?;
            }
            let lobby = LobbyModel::from_persisted(row);
            lobbies.insert(lobby.scope, lobby);
        }
        Ok(Self { store, lobbies })
    }

    #[must_use]
    pub const fn store(&self) -> &S {
        &self.store
    }

    #[must_use]
    pub fn get_lobby(&self, scope: LobbyScope) -> Option<&LobbyModel> {
        self.lobbies
            .get(&scope)
            .filter(|lobby| lobby.status.is_open())
    }

    pub fn get_or_create_lobby(
        &mut self,
        scope: LobbyScope,
        creator_id: Option<UserId>,
    ) -> Result<&LobbyModel, S::Error> {
        self.get_or_create_lobby_at(scope, creator_id, chrono::Utc::now().to_rfc3339())
    }

    pub fn get_or_create_lobby_at(
        &mut self,
        scope: LobbyScope,
        creator_id: Option<UserId>,
        created_at: impl Into<String>,
    ) -> Result<&LobbyModel, S::Error> {
        let existing_is_open = self
            .lobbies
            .get(&scope)
            .is_some_and(|lobby| lobby.status.is_open());
        if !existing_is_open {
            let lobby = LobbyModel::open(scope, creator_id.unwrap_or(UserId(0)), created_at);
            self.store.save_lobby_state(&lobby.to_persisted())?;
            self.lobbies.insert(scope, lobby);
        }
        Ok(self
            .lobbies
            .get(&scope)
            .expect("an open lobby was just inserted or already existed"))
    }

    pub fn join_lobby(
        &mut self,
        player_id: UserId,
        scope: LobbyScope,
    ) -> Result<LobbyJoinOutcome, S::Error> {
        let joined_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |duration| duration.as_secs_f64());
        self.join_lobby_at(player_id, scope, joined_at)
    }

    pub fn join_lobby_at(
        &mut self,
        player_id: UserId,
        scope: LobbyScope,
        joined_at: f64,
    ) -> Result<LobbyJoinOutcome, S::Error> {
        let mut candidate = self
            .get_lobby(scope)
            .cloned()
            .unwrap_or_else(|| LobbyModel::open(scope, UserId(0), chrono::Utc::now().to_rfc3339()));

        if candidate.total_count() >= MAX_LOBBY_PLAYERS {
            return Ok(LobbyJoinOutcome::Full);
        }
        if !candidate.add_player(player_id, joined_at) {
            return Ok(LobbyJoinOutcome::AlreadyJoined);
        }

        self.store.save_lobby_state(&candidate.to_persisted())?;
        self.lobbies.insert(scope, candidate);
        Ok(LobbyJoinOutcome::Ok)
    }

    pub fn leave_lobby(&mut self, player_id: UserId, scope: LobbyScope) -> Result<bool, S::Error> {
        let Some(mut candidate) = self.lobbies.get(&scope).cloned() else {
            return Ok(false);
        };
        if !candidate.remove_player(player_id) {
            return Ok(false);
        }

        self.store.save_lobby_state(&candidate.to_persisted())?;
        self.lobbies.insert(scope, candidate);
        Ok(true)
    }

    #[must_use]
    pub const fn join_lobby_conditional(
        &self,
        _player_id: UserId,
        _scope: LobbyScope,
    ) -> ConditionalLobbyJoinOutcome {
        ConditionalLobbyJoinOutcome::Deprecated
    }

    pub fn set_lobby_message(
        &mut self,
        scope: LobbyScope,
        message_ids: LobbyMessageIds,
    ) -> Result<bool, S::Error> {
        let Some(mut candidate) = self.lobbies.get(&scope).cloned() else {
            return Ok(false);
        };
        candidate.message_ids = message_ids;
        self.store.save_lobby_state(&candidate.to_persisted())?;
        self.lobbies.insert(scope, candidate);
        Ok(true)
    }

    #[must_use]
    pub fn lobby_message_ids(&self, scope: LobbyScope) -> Option<&LobbyMessageIds> {
        self.lobbies.get(&scope).map(|lobby| &lobby.message_ids)
    }

    pub fn reset_lobby(&mut self, scope: LobbyScope) -> Result<(), S::Error> {
        self.store
            .clear_lobby_state(scope.lobby_id(), Some(scope.guild_id.0))?;
        self.lobbies.remove(&scope);
        Ok(())
    }
}

#[cfg(test)]
#[path = "lobby_manager/tests.rs"]
mod tests;
