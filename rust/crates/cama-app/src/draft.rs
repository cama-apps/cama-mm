//! Discord-independent Immortal Draft state, policy, and publication orchestration.
//!
//! The Python command currently remains authoritative. This additive module
//! models the same state machine behind typed player, lobby, match, interaction,
//! and publication ports so the Rust path can be exercised without a gateway or
//! a second persistence authority.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard};

use cama_domain::player::Player;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DRAFT_POOL_SIZE: usize = 10;
pub const DRAFT_TOTAL_PICKS: usize = 8;
pub const BUTTON_LABEL_MAX_LENGTH: usize = 80;
pub const PRE_DRAFT_TIMEOUT_SECONDS: u64 = 300;
pub const DRAFTING_TIMEOUT_SECONDS: u64 = 600;

pub const DRAFT_COMMANDS: [&str; 4] = ["start", "restart", "sampleinprogress", "samplecomplete"];
pub const ADMIN_FAKE_PLAYER_ARGUMENTS: [&str; 4] = [
    "discord_id",
    "discord_username",
    "initial_mmr",
    "preferred_roles",
];

pub const AMBIGUOUS_LOBBY_MESSAGE: &str = concat!(
    "❓ You're queued in both 🍽️ All You Can Feed and 🧀 Whine & Cheese — ",
    "add the `lobby` option to pick one."
);

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LobbyKind {
    #[default]
    Open,
    #[serde(rename = "lowskill")]
    LowSkill,
}

impl LobbyKind {
    #[must_use]
    pub const fn value(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::LowSkill => "lowskill",
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Open => "All You Can Feed",
            Self::LowSkill => "Whine & Cheese",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "🍽️ All You Can Feed",
            Self::LowSkill => "🧀 Whine & Cheese",
        }
    }

    pub fn parse(value: Option<&str>) -> Result<Self, DraftError> {
        match value {
            None | Some("open") => Ok(Self::Open),
            Some("lowskill") => Ok(Self::LowSkill),
            Some(other) => Err(DraftError::new(format!("Unknown lobby kind: {other}"))),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftPhase {
    #[default]
    Coinflip,
    WinnerChoice,
    WinnerSideChoice,
    WinnerHeroChoice,
    LoserChoice,
    Drafting,
    Complete,
}

impl DraftPhase {
    #[must_use]
    pub const fn value(self) -> &'static str {
        match self {
            Self::Coinflip => "coinflip",
            Self::WinnerChoice => "winner_choice",
            Self::WinnerSideChoice => "winner_side_choice",
            Self::WinnerHeroChoice => "winner_hero_choice",
            Self::LoserChoice => "loser_choice",
            Self::Drafting => "drafting",
            Self::Complete => "complete",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PlayerPoolEntry {
    pub name: String,
    pub rating: f64,
    pub roles: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DraftState {
    pub session_id: u64,
    pub guild_id: i64,
    pub lobby_kind: LobbyKind,
    pub player_pool_ids: Vec<i64>,
    pub player_pool_data: BTreeMap<i64, PlayerPoolEntry>,
    pub excluded_player_ids: Vec<i64>,
    pub full_exclusion_increment_ids: Vec<i64>,
    pub half_exclusion_increment_ids: Vec<i64>,
    pub captain1_id: Option<i64>,
    pub captain2_id: Option<i64>,
    pub captain1_rating: f64,
    pub captain2_rating: f64,
    pub radiant_captain_id: Option<i64>,
    pub dire_captain_id: Option<i64>,
    pub coinflip_winner_id: Option<i64>,
    pub winner_choice_type: Option<String>,
    pub winner_choice_value: Option<String>,
    pub loser_choice_value: Option<String>,
    pub radiant_hero_pick_order: Option<u8>,
    pub dire_hero_pick_order: Option<u8>,
    pub current_round_first_captain_id: Option<i64>,
    pub current_pick_index: usize,
    pub radiant_player_ids: Vec<i64>,
    pub dire_player_ids: Vec<i64>,
    pub side_preferences: BTreeMap<i64, String>,
    pub phase: DraftPhase,
    pub draft_message_id: Option<i64>,
    pub draft_channel_id: Option<i64>,
    pub captain_ping_message_id: Option<i64>,
}

impl DraftState {
    #[must_use]
    pub fn new(guild_id: i64) -> Self {
        Self::with_session(guild_id, LobbyKind::Open, 0)
    }

    #[must_use]
    pub fn with_session(guild_id: i64, lobby_kind: LobbyKind, session_id: u64) -> Self {
        Self {
            session_id,
            guild_id,
            lobby_kind,
            player_pool_ids: Vec::new(),
            player_pool_data: BTreeMap::new(),
            excluded_player_ids: Vec::new(),
            full_exclusion_increment_ids: Vec::new(),
            half_exclusion_increment_ids: Vec::new(),
            captain1_id: None,
            captain2_id: None,
            captain1_rating: 0.0,
            captain2_rating: 0.0,
            radiant_captain_id: None,
            dire_captain_id: None,
            coinflip_winner_id: None,
            winner_choice_type: None,
            winner_choice_value: None,
            loser_choice_value: None,
            radiant_hero_pick_order: None,
            dire_hero_pick_order: None,
            current_round_first_captain_id: None,
            current_pick_index: 0,
            radiant_player_ids: Vec::new(),
            dire_player_ids: Vec::new(),
            side_preferences: BTreeMap::new(),
            phase: DraftPhase::Coinflip,
            draft_message_id: None,
            draft_channel_id: None,
            captain_ping_message_id: None,
        }
    }

    #[must_use]
    pub fn available_player_ids(&self) -> Vec<i64> {
        let picked = self
            .radiant_player_ids
            .iter()
            .chain(&self.dire_player_ids)
            .copied()
            .collect::<BTreeSet<_>>();
        let captains = [self.radiant_captain_id, self.dire_captain_id]
            .into_iter()
            .flatten()
            .collect::<BTreeSet<_>>();
        self.player_pool_ids
            .iter()
            .copied()
            .filter(|player_id| !picked.contains(player_id) && !captains.contains(player_id))
            .collect()
    }

    #[must_use]
    pub fn current_captain_id(&self) -> Option<i64> {
        if self.phase != DraftPhase::Drafting || self.current_pick_index >= DRAFT_TOTAL_PICKS {
            return None;
        }
        if self.current_pick_index.is_multiple_of(2) {
            self.current_round_first_captain_id
        } else {
            self.other_captain_id(self.current_round_first_captain_id)
        }
    }

    #[must_use]
    pub fn current_captain_team(&self) -> Option<&'static str> {
        let captain_id = self.current_captain_id()?;
        Some(if Some(captain_id) == self.radiant_captain_id {
            "radiant"
        } else {
            "dire"
        })
    }

    #[must_use]
    pub const fn picks_remaining_this_turn(&self) -> usize {
        if matches!(self.phase, DraftPhase::Drafting) && self.current_pick_index < DRAFT_TOTAL_PICKS
        {
            1
        } else {
            0
        }
    }

    fn player_rating(&self, player_id: i64) -> f64 {
        self.player_pool_data.get(&player_id).map_or_else(
            || {
                if Some(player_id) == self.captain1_id {
                    self.captain1_rating
                } else if Some(player_id) == self.captain2_id {
                    self.captain2_rating
                } else {
                    1500.0
                }
            },
            |data| data.rating,
        )
    }

    #[must_use]
    pub fn radiant_rating_total(&self) -> f64 {
        self.radiant_player_ids
            .iter()
            .map(|player_id| self.player_rating(*player_id))
            .sum()
    }

    #[must_use]
    pub fn dire_rating_total(&self) -> f64 {
        self.dire_player_ids
            .iter()
            .map(|player_id| self.player_rating(*player_id))
            .sum()
    }

    fn other_captain_id(&self, captain_id: Option<i64>) -> Option<i64> {
        if captain_id == self.radiant_captain_id {
            self.dire_captain_id
        } else if captain_id == self.dire_captain_id {
            self.radiant_captain_id
        } else {
            None
        }
    }

    fn choose_round_first_captain(&self) -> Option<i64> {
        match self
            .radiant_rating_total()
            .partial_cmp(&self.dire_rating_total())
            .unwrap_or(Ordering::Equal)
        {
            Ordering::Less => self.radiant_captain_id,
            Ordering::Greater => self.dire_captain_id,
            Ordering::Equal if self.current_pick_index == 0 => self
                .other_captain_id(self.coinflip_winner_id)
                .or(self.radiant_captain_id),
            Ordering::Equal => self
                .other_captain_id(self.current_round_first_captain_id)
                .or(self.radiant_captain_id),
        }
    }

    pub fn start_player_draft(&mut self) {
        if let Some(captain_id) = self.radiant_captain_id
            && !self.radiant_player_ids.contains(&captain_id)
        {
            self.radiant_player_ids.push(captain_id);
        }
        if let Some(captain_id) = self.dire_captain_id
            && !self.dire_player_ids.contains(&captain_id)
        {
            self.dire_player_ids.push(captain_id);
        }
        self.current_pick_index = 0;
        self.phase = DraftPhase::Drafting;
        self.current_round_first_captain_id = self.choose_round_first_captain();
    }

    #[must_use]
    pub fn is_draft_complete(&self) -> bool {
        self.current_pick_index >= DRAFT_TOTAL_PICKS
    }

    pub fn pick_player(&mut self, player_id: i64, picker_id: Option<i64>) -> bool {
        if self.phase != DraftPhase::Drafting {
            return false;
        }
        if picker_id.is_some() && picker_id != self.current_captain_id() {
            return false;
        }
        if !self.available_player_ids().contains(&player_id) {
            return false;
        }

        match self.current_captain_team() {
            Some("radiant") => self.radiant_player_ids.push(player_id),
            Some("dire") => self.dire_player_ids.push(player_id),
            _ => return false,
        }
        self.side_preferences.remove(&player_id);
        self.current_pick_index += 1;
        if self.is_draft_complete() {
            self.phase = DraftPhase::Complete;
        } else if self.current_pick_index.is_multiple_of(2) {
            self.current_round_first_captain_id = self.choose_round_first_captain();
        }
        true
    }

    pub fn set_side_preference(&mut self, player_id: i64, side: Option<&str>) -> bool {
        if !self.available_player_ids().contains(&player_id) {
            return false;
        }
        if let Some(side) = side {
            self.side_preferences.insert(player_id, side.to_owned());
        } else {
            self.side_preferences.remove(&player_id);
        }
        true
    }

    #[must_use]
    pub fn to_snapshot(&self) -> DraftStateSnapshot {
        DraftStateSnapshot(self.clone())
    }

    #[must_use]
    pub fn from_snapshot(snapshot: DraftStateSnapshot) -> Self {
        snapshot.0
    }
}

/// Version of the serde envelope stored by the draft persistence boundary.
pub const DRAFT_STATE_SCHEMA_VERSION: u64 = cama_db::draft_state::DRAFT_STATE_ENVELOPE_VERSION;

const fn default_draft_active() -> bool {
    true
}

/// Complete, serde-compatible snapshot of the authoritative draft state.
///
/// The newtype keeps the pre-existing `DraftStateSnapshot` API while making
/// every `DraftState` field durable.  It intentionally contains no derived
/// or publication-only state.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DraftStateSnapshot(DraftState);

impl DraftStateSnapshot {
    /// Borrow the complete authoritative state.
    #[must_use]
    pub const fn as_state(&self) -> &DraftState {
        &self.0
    }

    /// Consume this snapshot into the authoritative state.
    #[must_use]
    pub fn into_state(self) -> DraftState {
        self.0
    }

    /// Wrap this snapshot in the durable revision envelope used by the
    /// SQLite adapter and future runtime ports.
    #[must_use]
    pub fn envelope(&self, revision: u64) -> DraftStateEnvelope {
        DraftStateEnvelope::new(self.clone(), revision)
    }

    /// Encode the complete snapshot as its application-owned JSON payload.
    pub fn to_json(&self) -> Result<String, DraftPersistenceError> {
        serde_json::to_string(self).map_err(|error| DraftPersistenceError::Serialization {
            message: error.to_string(),
        })
    }

    /// Decode an application-owned JSON payload into a complete snapshot.
    pub fn from_json(raw: &str) -> Result<Self, DraftPersistenceError> {
        serde_json::from_str(raw).map_err(|error| DraftPersistenceError::Serialization {
            message: error.to_string(),
        })
    }
}

/// Durable metadata plus the complete typed draft snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DraftStateEnvelope {
    /// Envelope format version, independent of the application payload.
    #[serde(alias = "version")]
    pub schema_version: u64,
    /// Guild key used in `app_kv`.
    pub guild_id: i64,
    /// Monotonic identity allocated by the database adapter.
    pub session_id: u64,
    /// Optimistic-concurrency revision, starting at one.
    pub revision: u64,
    /// Original pre-draft/view deadline when one has been installed.  It is
    /// optional in schema v1 so old envelopes remain loadable.
    #[serde(default)]
    pub deadline_at: Option<i64>,
    /// Whether this envelope still owns the guild's active draft slot.
    /// Missing legacy metadata defaults to active for safe recovery.
    #[serde(default = "default_draft_active")]
    pub active: bool,
    /// Whether completion is finalizing its pending match/publication.
    #[serde(default)]
    pub finalizing: bool,
    /// Pending match identity, when completion has crossed the match-create
    /// boundary but cleanup/publication has not finished.
    #[serde(default)]
    pub pending_match_id: Option<i64>,
    /// The complete authoritative state snapshot.
    pub state: DraftStateSnapshot,
    /// Forward-compatible application metadata unknown to this adapter
    /// revision. Flattening retains future top-level fields across typed
    /// load/replace cycles instead of silently discarding them.
    #[serde(default, flatten)]
    pub application_metadata: BTreeMap<String, serde_json::Value>,
}

impl DraftStateEnvelope {
    /// Construct a new revision envelope around a complete state snapshot.
    #[must_use]
    pub fn new(state: DraftStateSnapshot, revision: u64) -> Self {
        Self {
            schema_version: DRAFT_STATE_SCHEMA_VERSION,
            guild_id: state.as_state().guild_id,
            session_id: state.as_state().session_id,
            revision,
            deadline_at: None,
            active: true,
            finalizing: false,
            pending_match_id: None,
            state,
            application_metadata: BTreeMap::new(),
        }
    }

    /// Validate the envelope metadata against its embedded authoritative
    /// state before it crosses a persistence or runtime boundary.
    pub fn validate(&self) -> Result<(), DraftPersistenceError> {
        if self.schema_version != DRAFT_STATE_SCHEMA_VERSION {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: format!(
                    "unsupported draft state schema version {}",
                    self.schema_version
                ),
            });
        }
        if self.revision == 0 {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: "draft state revision must be positive".to_owned(),
            });
        }
        if self.guild_id != self.state.as_state().guild_id {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: format!(
                    "envelope guild {} does not match state guild {}",
                    self.guild_id,
                    self.state.as_state().guild_id
                ),
            });
        }
        if self.session_id != self.state.as_state().session_id {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: format!(
                    "envelope session {} does not match state session {}",
                    self.session_id,
                    self.state.as_state().session_id
                ),
            });
        }
        if let Some(reserved) = self.application_metadata.keys().find(|key| {
            matches!(
                key.as_str(),
                "schema_version"
                    | "version"
                    | "guild_id"
                    | "session_id"
                    | "revision"
                    | "deadline_at"
                    | "active"
                    | "finalizing"
                    | "pending_match_id"
                    | "state"
            )
        }) {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: format!("application metadata uses reserved key {reserved}"),
            });
        }
        Ok(())
    }

    /// Encode the envelope using the stable persisted JSON representation.
    pub fn to_json(&self) -> Result<String, DraftPersistenceError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| DraftPersistenceError::Serialization {
            message: error.to_string(),
        })
    }

    /// Decode and validate a persisted envelope.
    pub fn from_json(raw: &str) -> Result<Self, DraftPersistenceError> {
        let envelope: Self =
            serde_json::from_str(raw).map_err(|error| DraftPersistenceError::Serialization {
                message: error.to_string(),
            })?;
        envelope.validate()?;
        Ok(envelope)
    }
}

/// Errors crossing the draft persistence port.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DraftPersistenceError {
    /// The backing repository rejected an operation for a non-CAS reason.
    #[error("draft persistence backend failed: {message}")]
    Backend { message: String },
    /// The supplied or loaded envelope failed validation.
    #[error("draft persistence envelope is invalid: {message}")]
    InvalidEnvelope { message: String },
    /// JSON encoding/decoding failed.
    #[error("draft persistence JSON failed: {message}")]
    Serialization { message: String },
    /// A guild already has a durable active draft.
    #[error("draft already exists for guild {guild_id}")]
    Duplicate { guild_id: i64 },
    /// No durable draft exists for the requested guild.
    #[error("draft does not exist for guild {guild_id}")]
    NotFound { guild_id: i64 },
    /// The caller's CAS revision is stale.
    #[error("draft revision is stale for guild {guild_id}: expected {expected}, found {actual:?}")]
    StaleRevision {
        guild_id: i64,
        expected: u64,
        actual: Option<u64>,
    },
    /// The callback's session identity is no longer active for the guild.
    #[error("draft session is stale for guild {guild_id}: expected {expected}, found {actual:?}")]
    StaleSession {
        guild_id: i64,
        expected: u64,
        actual: Option<u64>,
    },
    /// The repository could not complete an otherwise valid CAS operation.
    #[error("draft persistence conflict for guild {guild_id}: {reason}")]
    Conflict { guild_id: i64, reason: String },
}

impl From<cama_db::draft_state::DraftStateRepositoryError> for DraftPersistenceError {
    fn from(error: cama_db::draft_state::DraftStateRepositoryError) -> Self {
        use cama_db::draft_state::DraftStateRepositoryError as DatabaseError;
        match error {
            DatabaseError::Duplicate { guild_id } => Self::Duplicate { guild_id },
            DatabaseError::NotFound { guild_id } => Self::NotFound { guild_id },
            DatabaseError::StaleRevision {
                guild_id,
                expected,
                actual,
            } => Self::StaleRevision {
                guild_id,
                expected,
                actual,
            },
            DatabaseError::StaleSession {
                guild_id,
                expected,
                actual,
            } => Self::StaleSession {
                guild_id,
                expected,
                actual,
            },
            DatabaseError::Conflict { guild_id, reason } => Self::Conflict { guild_id, reason },
            DatabaseError::InvalidEnvelope(message) => Self::InvalidEnvelope { message },
            DatabaseError::InvalidPayload(message) => Self::Serialization { message },
            other => Self::Backend {
                message: other.to_string(),
            },
        }
    }
}

/// Persistence boundary needed by the future gateway/runtime owner.
///
/// Runtime/provider/READY code is intentionally not coupled to this trait in
/// the first persistence tranche.
pub trait DraftStatePersistencePort: Send + Sync {
    /// Load all active durable draft envelopes.
    fn load_all(&self) -> Result<Vec<DraftStateEnvelope>, DraftPersistenceError>;

    /// Allocate a session and create a new active draft.
    fn create(
        &self,
        snapshot: &DraftStateSnapshot,
    ) -> Result<DraftStateEnvelope, DraftPersistenceError>;

    /// Allocate a session and create a snapshot plus recovery metadata in one
    /// atomic row write.
    fn create_envelope(
        &self,
        envelope: &DraftStateEnvelope,
    ) -> Result<DraftStateEnvelope, DraftPersistenceError>;

    /// Replace a snapshot under an optimistic-concurrency revision guard.
    fn replace_if_revision(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        snapshot: &DraftStateSnapshot,
    ) -> Result<DraftStateEnvelope, DraftPersistenceError>;

    /// Replace a complete snapshot plus recovery metadata under the same
    /// session-and-revision guard.
    fn replace_envelope_if_revision(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        envelope: &DraftStateEnvelope,
    ) -> Result<DraftStateEnvelope, DraftPersistenceError>;

    /// Delete a snapshot under an optimistic-concurrency revision guard.
    fn delete_if_revision(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
    ) -> Result<bool, DraftPersistenceError>;
}

/// Short compatibility alias for callers that refer to the boundary as the
/// generic draft persistence port.
pub use DraftStatePersistencePort as DraftPersistencePort;

/// Existing-schema SQLite implementation of [`DraftStatePersistencePort`].
///
/// This adapter is deliberately not installed into `DraftStateManager`; the
/// runtime cutover can choose when to hydrate and publish it.
#[derive(Clone, Debug)]
pub struct SqliteDraftStatePersistence {
    repository: cama_db::draft_state::DraftStateRepository,
}

/// Short compatibility alias for the SQLite draft persistence adapter.
pub type SqliteDraftPersistence = SqliteDraftStatePersistence;

impl SqliteDraftStatePersistence {
    /// Build an adapter for an already migrated database.
    #[must_use]
    pub fn new(path: impl AsRef<std::path::Path>) -> Self {
        Self {
            repository: cama_db::draft_state::DraftStateRepository::new(path),
        }
    }

    fn decode(
        record: cama_db::draft_state::DraftStateRecord,
    ) -> Result<DraftStateEnvelope, DraftPersistenceError> {
        let envelope = DraftStateEnvelope::from_json(&record.envelope_json)?;
        if envelope.guild_id != record.guild_id
            || envelope.session_id != record.session_id
            || envelope.revision != record.revision
        {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: "database row metadata does not match its envelope".to_owned(),
            });
        }
        Ok(envelope)
    }
}

impl DraftStatePersistencePort for SqliteDraftStatePersistence {
    fn load_all(&self) -> Result<Vec<DraftStateEnvelope>, DraftPersistenceError> {
        self.repository
            .load_all()?
            .into_iter()
            .map(Self::decode)
            .collect()
    }

    fn create(
        &self,
        snapshot: &DraftStateSnapshot,
    ) -> Result<DraftStateEnvelope, DraftPersistenceError> {
        self.create_envelope(&snapshot.envelope(1))
    }

    fn create_envelope(
        &self,
        envelope: &DraftStateEnvelope,
    ) -> Result<DraftStateEnvelope, DraftPersistenceError> {
        envelope.validate()?;
        let envelope_json = envelope.to_json()?;
        // The database owns session allocation and stamps the embedded state
        // session field in the same transaction.
        Self::decode(
            self.repository
                .create_envelope(envelope.guild_id, &envelope_json)?,
        )
    }

    fn replace_if_revision(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        snapshot: &DraftStateSnapshot,
    ) -> Result<DraftStateEnvelope, DraftPersistenceError> {
        if snapshot.as_state().guild_id != guild_id {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "snapshot guild does not match CAS guild".to_owned(),
            });
        }
        let state_json = snapshot.to_json()?;
        Self::decode(self.repository.replace_if_revision(
            guild_id,
            expected_session_id,
            expected_revision,
            &state_json,
        )?)
    }

    fn replace_envelope_if_revision(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        envelope: &DraftStateEnvelope,
    ) -> Result<DraftStateEnvelope, DraftPersistenceError> {
        if envelope.guild_id != guild_id || envelope.state.as_state().guild_id != guild_id {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "envelope guild does not match CAS guild".to_owned(),
            });
        }
        envelope.validate()?;
        let envelope_json = envelope.to_json()?;
        Self::decode(self.repository.replace_envelope_if_revision(
            guild_id,
            expected_session_id,
            expected_revision,
            &envelope_json,
        )?)
    }

    fn delete_if_revision(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
    ) -> Result<bool, DraftPersistenceError> {
        Ok(self
            .repository
            .delete_if_revision(guild_id, expected_session_id, expected_revision)?)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PendingMatchState {
    pub pending_match_id: Option<i64>,
    pub lobby_kind: String,
    pub radiant_team_ids: Vec<i64>,
    pub dire_team_ids: Vec<i64>,
    pub excluded_player_ids: Vec<i64>,
    pub excluded_conditional_player_ids: Vec<i64>,
    pub exclusion_updates_deferred: bool,
    pub full_exclusion_increment_ids: Vec<i64>,
    pub half_exclusion_increment_ids: Vec<i64>,
    pub radiant_value: f64,
    pub dire_value: f64,
    pub value_diff: f64,
    pub first_pick_team: String,
    pub shuffle_timestamp: i64,
    pub bet_lock_until: i64,
    pub shuffle_message_id: Option<i64>,
    pub shuffle_channel_id: Option<i64>,
    pub shuffle_message_jump_url: Option<String>,
    pub origin_channel_id: Option<i64>,
    pub thread_shuffle_thread_id: Option<i64>,
    pub thread_shuffle_message_id: Option<i64>,
    pub is_draft: bool,
    pub is_bomb_pot: bool,
}

impl PendingMatchState {
    #[must_use]
    pub fn to_snapshot(&self) -> PendingMatchSnapshot {
        PendingMatchSnapshot(self.clone())
    }

    #[must_use]
    pub fn from_snapshot(snapshot: PendingMatchSnapshot) -> Self {
        snapshot.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingMatchSnapshot(PendingMatchState);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct DraftError {
    pub message: String,
}

impl DraftError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub type DraftHandle = Arc<Mutex<DraftState>>;

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Default)]
pub struct DraftStateManager {
    states: Mutex<BTreeMap<i64, DraftHandle>>,
    next_session_id: AtomicU64,
}

impl DraftStateManager {
    #[must_use]
    pub fn get_state(&self, guild_id: Option<i64>) -> Option<DraftHandle> {
        lock_recover(&self.states)
            .get(&guild_id.unwrap_or(0))
            .cloned()
    }

    pub fn create_draft(
        &self,
        guild_id: Option<i64>,
        lobby_kind: LobbyKind,
    ) -> Result<DraftHandle, DraftError> {
        let guild_id = guild_id.unwrap_or(0);
        let mut states = lock_recover(&self.states);
        if states.contains_key(&guild_id) {
            return Err(DraftError::new(
                "A draft is already in progress for this server.",
            ));
        }
        let session_id = self.next_session_id.fetch_add(1, AtomicOrdering::Relaxed) + 1;
        let state = Arc::new(Mutex::new(DraftState::with_session(
            guild_id, lobby_kind, session_id,
        )));
        states.insert(guild_id, Arc::clone(&state));
        Ok(state)
    }

    /// Restore a durable draft whose session identity was allocated by the
    /// database.
    ///
    /// Runtime hydration must not call [`Self::create_draft`] and then swap
    /// identities: component callbacks can observe the manager between those
    /// operations.  Inserting the complete state atomically keeps the
    /// database-owned session, phase, message ids, and deadline-associated
    /// state together from the first gateway callback.
    pub fn restore_draft(&self, state: DraftState) -> Result<DraftHandle, DraftError> {
        let guild_id = state.guild_id;
        if state.session_id == 0 {
            return Err(DraftError::new(
                "A restored draft must have a database-allocated session.",
            ));
        }
        let mut states = lock_recover(&self.states);
        if states.contains_key(&guild_id) {
            return Err(DraftError::new(
                "A draft is already in progress for this server.",
            ));
        }

        // Keep process-local allocations above any restored identity before
        // publishing the handle in `states`; another guild can otherwise
        // allocate an older session in the small hydration window.
        let mut next = self.next_session_id.load(AtomicOrdering::Relaxed);
        while next < state.session_id {
            match self.next_session_id.compare_exchange_weak(
                next,
                state.session_id,
                AtomicOrdering::Relaxed,
                AtomicOrdering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => next = observed,
            }
        }
        let handle = Arc::new(Mutex::new(state.clone()));
        states.insert(guild_id, Arc::clone(&handle));
        Ok(handle)
    }

    pub fn clear_state(
        &self,
        guild_id: Option<i64>,
        expected_state: Option<&DraftHandle>,
    ) -> Option<DraftHandle> {
        let guild_id = guild_id.unwrap_or(0);
        let mut states = lock_recover(&self.states);
        let current = states.get(&guild_id)?;
        if expected_state.is_some_and(|expected| !Arc::ptr_eq(current, expected)) {
            return None;
        }
        states.remove(&guild_id)
    }

    #[must_use]
    pub fn has_active_draft(&self, guild_id: Option<i64>) -> bool {
        self.get_state(guild_id).is_some()
    }

    pub fn advance_phase(&self, guild_id: Option<i64>, phase: DraftPhase) -> bool {
        let Some(state) = self.get_state(guild_id) else {
            return false;
        };
        lock_recover(&state).phase = phase;
        true
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaptainPair {
    pub captain1_id: i64,
    pub captain1_rating: f64,
    pub captain2_id: i64,
    pub captain2_rating: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolSelectionResult {
    pub selected_ids: Vec<i64>,
    pub excluded_ids: Vec<i64>,
}

pub trait DraftCoinPort: Send {
    fn choose(&mut self, captain1_id: i64, captain2_id: i64) -> i64;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AlternatingDraftCoin {
    choose_second: bool,
}

impl DraftCoinPort for AlternatingDraftCoin {
    fn choose(&mut self, captain1_id: i64, captain2_id: i64) -> i64 {
        let choice = if self.choose_second {
            captain2_id
        } else {
            captain1_id
        };
        self.choose_second = !self.choose_second;
        choice
    }
}

pub struct DraftService {
    coin: Box<dyn DraftCoinPort>,
}

impl Default for DraftService {
    fn default() -> Self {
        Self::new()
    }
}

impl DraftService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            coin: Box::new(AlternatingDraftCoin::default()),
        }
    }

    #[must_use]
    pub fn with_coin(coin: impl DraftCoinPort + 'static) -> Self {
        Self {
            coin: Box::new(coin),
        }
    }

    pub fn select_captains(
        &self,
        player_pool_ids: &[i64],
        player_ratings: &BTreeMap<i64, f64>,
        specified_captain1: Option<i64>,
        specified_captain2: Option<i64>,
    ) -> Result<CaptainPair, DraftError> {
        if let (Some(captain1_id), Some(captain2_id)) = (specified_captain1, specified_captain2) {
            return Ok(CaptainPair {
                captain1_id,
                captain1_rating: player_ratings.get(&captain1_id).copied().unwrap_or(0.0),
                captain2_id,
                captain2_rating: player_ratings.get(&captain2_id).copied().unwrap_or(0.0),
            });
        }

        let available = player_pool_ids
            .iter()
            .copied()
            .filter(|player_id| {
                Some(*player_id) != specified_captain1 && Some(*player_id) != specified_captain2
            })
            .collect::<Vec<_>>();
        let (captain1_id, captain2_id) = match (specified_captain1, specified_captain2) {
            (None, None) => {
                if available.len() < 2 {
                    return Err(DraftError::new(format!(
                        "Need at least 2 players in the draft pool, but only {} available.",
                        available.len()
                    )));
                }
                let mut best_pair = None;
                let mut best_difference = f64::INFINITY;
                for (index, captain1_id) in available.iter().copied().enumerate() {
                    let captain1_rating = player_ratings.get(&captain1_id).copied().unwrap_or(0.0);
                    for captain2_id in available[index + 1..].iter().copied() {
                        let difference = (captain1_rating
                            - player_ratings.get(&captain2_id).copied().unwrap_or(0.0))
                        .abs();
                        if difference < best_difference {
                            best_difference = difference;
                            best_pair = Some((captain1_id, captain2_id));
                        }
                    }
                }
                best_pair
                    .ok_or_else(|| DraftError::new("Need at least 2 players in the draft pool."))?
            }
            (None, Some(captain2_id)) => {
                let captain1_id = closest_rating_captain(captain2_id, &available, player_ratings)?;
                (captain1_id, captain2_id)
            }
            (Some(captain1_id), None) => {
                let captain2_id = closest_rating_captain(captain1_id, &available, player_ratings)?;
                (captain1_id, captain2_id)
            }
            (Some(_), Some(_)) => unreachable!("handled above"),
        };

        Ok(CaptainPair {
            captain1_id,
            captain1_rating: player_ratings.get(&captain1_id).copied().unwrap_or(0.0),
            captain2_id,
            captain2_rating: player_ratings.get(&captain2_id).copied().unwrap_or(0.0),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn select_player_pool(
        &self,
        regular_player_ids: &[i64],
        conditional_player_ids: &[i64],
        exclusion_counts: &BTreeMap<i64, i64>,
        player_ratings: &BTreeMap<i64, f64>,
        forced_include_ids: &[i64],
        pool_size: usize,
    ) -> Result<PoolSelectionResult, DraftError> {
        let all_player_ids = deduplicated(
            regular_player_ids
                .iter()
                .chain(conditional_player_ids)
                .copied(),
        );
        if all_player_ids.len() < pool_size {
            return Err(DraftError::new(format!(
                "Need at least {pool_size} players in lobby, but only {} present.",
                all_player_ids.len()
            )));
        }
        let forced = deduplicated(forced_include_ids.iter().copied());
        if forced.len() > pool_size {
            return Err(DraftError::new(
                "Too many forced-include players for pool size.",
            ));
        }
        if forced
            .iter()
            .any(|player_id| !all_player_ids.contains(player_id))
        {
            return Err(DraftError::new(
                "Specified captains must be present in the lobby.",
            ));
        }
        let forced_set = forced.iter().copied().collect::<BTreeSet<_>>();
        let mut selected = forced;
        for group in [regular_player_ids, conditional_player_ids] {
            let mut ranked = deduplicated(group.iter().copied())
                .into_iter()
                .enumerate()
                .filter(|(_, player_id)| !forced_set.contains(player_id))
                .collect::<Vec<_>>();
            ranked.sort_by(|(left_index, left_id), (right_index, right_id)| {
                exclusion_counts
                    .get(right_id)
                    .copied()
                    .unwrap_or(0)
                    .cmp(&exclusion_counts.get(left_id).copied().unwrap_or(0))
                    .then_with(|| {
                        player_ratings
                            .get(right_id)
                            .copied()
                            .unwrap_or(1500.0)
                            .partial_cmp(&player_ratings.get(left_id).copied().unwrap_or(1500.0))
                            .unwrap_or(Ordering::Equal)
                    })
                    .then_with(|| left_index.cmp(right_index))
            });
            for (_, player_id) in ranked {
                if selected.len() >= pool_size {
                    break;
                }
                selected.push(player_id);
            }
        }
        let selected_set = selected.iter().copied().collect::<BTreeSet<_>>();
        let excluded_ids = all_player_ids
            .into_iter()
            .filter(|player_id| !selected_set.contains(player_id))
            .collect();
        Ok(PoolSelectionResult {
            selected_ids: selected,
            excluded_ids,
        })
    }

    pub fn coinflip(&mut self, captain1_id: i64, captain2_id: i64) -> i64 {
        self.coin.choose(captain1_id, captain2_id)
    }
}

fn closest_rating_captain(
    reference_captain_id: i64,
    candidates: &[i64],
    player_ratings: &BTreeMap<i64, f64>,
) -> Result<i64, DraftError> {
    let Some(first) = candidates.first().copied() else {
        return Err(DraftError::new(
            "Need at least 1 other player in the draft pool.",
        ));
    };
    let reference_rating = player_ratings
        .get(&reference_captain_id)
        .copied()
        .unwrap_or(0.0);
    Ok(candidates
        .iter()
        .copied()
        .skip(1)
        .fold(first, |best, candidate| {
            let best_difference =
                (player_ratings.get(&best).copied().unwrap_or(0.0) - reference_rating).abs();
            let candidate_difference =
                (player_ratings.get(&candidate).copied().unwrap_or(0.0) - reference_rating).abs();
            if candidate_difference < best_difference {
                candidate
            } else {
                best
            }
        }))
}

fn deduplicated(values: impl IntoIterator<Item = i64>) -> Vec<i64> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(*value))
        .collect()
}

pub trait DraftPlayerPort {
    fn get_by_ids(&mut self, player_ids: &[i64], guild_id: i64) -> Vec<Player>;
    fn get_by_id(&mut self, player_id: i64, guild_id: i64) -> Option<Player>;
    fn exclusion_counts(&mut self, player_ids: &[i64], guild_id: i64) -> BTreeMap<i64, i64>;
    fn get_captain_eligible(&mut self, player_id: i64, guild_id: i64) -> bool;
    fn set_captain_eligible(&mut self, player_id: i64, guild_id: i64, eligible: bool) -> bool;
    fn get_captain_eligible_players(&mut self, player_ids: &[i64], guild_id: i64) -> Vec<i64>;
}

#[derive(Clone, Debug, Default)]
pub struct MemoryDraftPlayers {
    players: BTreeMap<(i64, i64), Player>,
    exclusion_counts: BTreeMap<(i64, i64), i64>,
    captain_eligible: BTreeSet<(i64, i64)>,
}

impl MemoryDraftPlayers {
    pub fn add(&mut self, guild_id: i64, mut player: Player) -> Result<(), DraftError> {
        let player_id = player
            .discord_id
            .ok_or_else(|| DraftError::new("Draft players require a Discord ID."))?;
        player.guild_id = Some(guild_id);
        self.players.insert((guild_id, player_id), player);
        Ok(())
    }

    pub fn set_exclusion_count(&mut self, guild_id: i64, player_id: i64, count: i64) {
        self.exclusion_counts.insert((guild_id, player_id), count);
    }
}

impl DraftPlayerPort for MemoryDraftPlayers {
    fn get_by_ids(&mut self, player_ids: &[i64], guild_id: i64) -> Vec<Player> {
        player_ids
            .iter()
            .filter_map(|player_id| self.players.get(&(guild_id, *player_id)).cloned())
            .collect()
    }

    fn get_by_id(&mut self, player_id: i64, guild_id: i64) -> Option<Player> {
        self.players.get(&(guild_id, player_id)).cloned()
    }

    fn exclusion_counts(&mut self, player_ids: &[i64], guild_id: i64) -> BTreeMap<i64, i64> {
        player_ids
            .iter()
            .map(|player_id| {
                (
                    *player_id,
                    self.exclusion_counts
                        .get(&(guild_id, *player_id))
                        .copied()
                        .unwrap_or(0),
                )
            })
            .collect()
    }

    fn get_captain_eligible(&mut self, player_id: i64, guild_id: i64) -> bool {
        self.captain_eligible.contains(&(guild_id, player_id))
    }

    fn set_captain_eligible(&mut self, player_id: i64, guild_id: i64, eligible: bool) -> bool {
        if !self.players.contains_key(&(guild_id, player_id)) {
            return false;
        }
        if eligible {
            self.captain_eligible.insert((guild_id, player_id));
        } else {
            self.captain_eligible.remove(&(guild_id, player_id));
        }
        true
    }

    fn get_captain_eligible_players(&mut self, player_ids: &[i64], guild_id: i64) -> Vec<i64> {
        player_ids
            .iter()
            .copied()
            .filter(|player_id| self.captain_eligible.contains(&(guild_id, *player_id)))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftButtonStyle {
    Primary,
    Secondary,
    Success,
    Danger,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftButtonModel {
    pub label: String,
    pub custom_id: String,
    pub emoji: Option<String>,
    pub style: DraftButtonStyle,
    pub row: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftViewModel {
    pub timeout_seconds: u64,
    pub current_captain_id: Option<i64>,
    pub participant_ids: BTreeSet<i64>,
    pub buttons: Vec<DraftButtonModel>,
    pub state_session_id: Option<u64>,
}

impl DraftViewModel {
    #[must_use]
    pub fn drafting(
        available_players: &[Player],
        current_captain_id: Option<i64>,
        captain_ids: impl IntoIterator<Item = i64>,
        player_pool_ids: &[i64],
        state_session_id: Option<u64>,
    ) -> Self {
        let mut buttons = available_players
            .iter()
            .take(8)
            .enumerate()
            .filter_map(|(index, player)| {
                let player_id = player.discord_id?;
                let roles = format_roles(player.preferred_roles.as_deref());
                let label = format!(
                    "{} ({:.0}) {}",
                    player.name,
                    player.glicko_rating.unwrap_or(1500.0),
                    roles
                )
                .trim()
                .chars()
                .take(BUTTON_LABEL_MAX_LENGTH)
                .collect();
                Some(DraftButtonModel {
                    label,
                    custom_id: format!("draft_pick_{player_id}"),
                    emoji: None,
                    style: DraftButtonStyle::Primary,
                    row: u8::try_from(index / 4).unwrap_or(0),
                })
            })
            .collect::<Vec<_>>();
        buttons.extend([
            DraftButtonModel {
                label: "Prefer Radiant".to_owned(),
                custom_id: "draft_pref_radiant".to_owned(),
                emoji: Some("🟢".to_owned()),
                style: DraftButtonStyle::Success,
                row: 4,
            },
            DraftButtonModel {
                label: "Prefer Dire".to_owned(),
                custom_id: "draft_pref_dire".to_owned(),
                emoji: Some("🔴".to_owned()),
                style: DraftButtonStyle::Danger,
                row: 4,
            },
            DraftButtonModel {
                label: "Clear Preference".to_owned(),
                custom_id: "draft_pref_clear".to_owned(),
                emoji: Some("❌".to_owned()),
                style: DraftButtonStyle::Secondary,
                row: 4,
            },
        ]);
        let participant_ids = captain_ids
            .into_iter()
            .chain(player_pool_ids.iter().copied())
            .collect();
        Self {
            timeout_seconds: DRAFTING_TIMEOUT_SECONDS,
            current_captain_id,
            participant_ids,
            buttons,
            state_session_id,
        }
    }

    #[must_use]
    pub fn interaction_check(&self, user_id: i64) -> DraftInteractionDecision {
        if self.participant_ids.is_empty() || self.participant_ids.contains(&user_id) {
            DraftInteractionDecision::Allowed
        } else {
            DraftInteractionDecision::Rejected(DraftResponse::ephemeral(
                "❌ You are not part of this draft.",
            ))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftInteractionDecision {
    Allowed,
    Rejected(DraftResponse),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftResponse {
    pub content: String,
    pub ephemeral: bool,
}

impl DraftResponse {
    #[must_use]
    pub fn public(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ephemeral: false,
        }
    }

    #[must_use]
    pub fn ephemeral(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ephemeral: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftEmbed {
    pub title: String,
    pub description: String,
    pub footer: Option<String>,
    pub fields: Vec<DraftEmbedField>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftEmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftMessagePayload {
    pub content: Option<String>,
    pub embed: Option<DraftEmbed>,
    pub view: Option<DraftViewModel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedDraftMessage {
    pub id: i64,
    pub channel_id: i64,
    pub jump_url: Option<String>,
}

pub fn format_roles(roles: Option<&[String]>) -> String {
    let Some(roles) = roles.filter(|roles| !roles.is_empty()) else {
        return String::new();
    };
    let mut indexed = roles.iter().cloned().enumerate().collect::<Vec<_>>();
    indexed.sort_by_key(|(index, role)| (role.parse::<i64>().unwrap_or(99), *index));
    format!(
        "[{}]",
        indexed
            .into_iter()
            .map(|(_, role)| role)
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub fn format_player_row(is_captain: bool, name: &str, rating: f64, roles: &str) -> String {
    let icon = if is_captain { "👑" } else { "⠀⠀" };
    let name = name.chars().take(10).collect::<String>();
    let roles = if roles.is_empty() {
        String::new()
    } else {
        format!(" {roles}")
    };
    format!("{icon} {name} ({rating:.0}){roles}")
}

#[must_use]
pub fn build_player_pool_field(state: &DraftState) -> String {
    let mut player_info = state
        .player_pool_ids
        .iter()
        .copied()
        .filter(|player_id| {
            Some(*player_id) != state.captain1_id && Some(*player_id) != state.captain2_id
        })
        .enumerate()
        .map(|(index, player_id)| {
            let data = state
                .player_pool_data
                .get(&player_id)
                .cloned()
                .unwrap_or_else(|| PlayerPoolEntry {
                    name: format!("Player {player_id}"),
                    rating: 1500.0,
                    roles: Vec::new(),
                });
            (index, data)
        })
        .collect::<Vec<_>>();
    if player_info.is_empty() {
        return "No players in pool".to_owned();
    }
    player_info.sort_by(|(left_index, left), (right_index, right)| {
        right
            .rating
            .partial_cmp(&left.rating)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left_index.cmp(right_index))
    });
    player_info
        .into_iter()
        .map(|(_, player)| {
            format!(
                "{} ({:.0}) {}",
                player.name,
                player.rating,
                format_roles(Some(&player.roles))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn resolve_start_lobby_kind(
    memberships: &[LobbyKind],
    explicit: Option<LobbyKind>,
) -> Result<LobbyKind, DraftResponse> {
    if let Some(explicit) = explicit {
        return Ok(explicit);
    }
    match memberships {
        [] => Err(DraftResponse::ephemeral(
            "❌ Join a lobby before using `/draft start`.",
        )),
        [only] => Ok(*only),
        _ => Err(DraftResponse::ephemeral(AMBIGUOUS_LOBBY_MESSAGE)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShuffleRoutingResult {
    pub draft_called: bool,
    pub response: Option<DraftResponse>,
}

#[must_use]
pub fn route_large_lobby_shuffle(
    player_ids: &[i64],
    pending_player_ids: &BTreeSet<i64>,
) -> ShuffleRoutingResult {
    let blocked = player_ids
        .iter()
        .any(|player_id| pending_player_ids.contains(player_id));
    ShuffleRoutingResult {
        draft_called: false,
        response: blocked.then(|| {
            DraftResponse::ephemeral(
                "❌ Cannot shuffle: one or more selected players are already in a pending match.",
            )
        }),
    }
}

pub trait DraftLobbyPort {
    fn reserve_lobby_players(
        &mut self,
        player_ids: &[i64],
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<bool, DraftError>;
    fn release_lobby_players(
        &mut self,
        player_ids: &[i64],
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<(), DraftError>;
    fn reset_lobby(&mut self, guild_id: i64, lobby_kind: LobbyKind) -> Result<(), DraftError>;
    fn lobby_channel_id(&mut self, guild_id: i64, lobby_kind: LobbyKind) -> Option<i64>;
    fn origin_channel_id(&mut self, guild_id: i64, lobby_kind: LobbyKind) -> Option<i64>;
    fn lobby_thread_id(&mut self, guild_id: i64, lobby_kind: LobbyKind) -> Option<i64>;
    fn evict_from_other_lobbies(
        &mut self,
        player_ids: &[i64],
        guild_id: i64,
        popped_kind: LobbyKind,
    ) -> Result<(), DraftError>;
    fn clear_rally_cooldown(
        &mut self,
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<(), DraftError>;
    fn refresh_bonus_pool_lobbies(&mut self, guild_id: i64) -> Result<(), DraftError>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftMessageInfo {
    pub message_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub jump_url: Option<String>,
    pub thread_message_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub origin_channel_id: Option<i64>,
    pub pending_match_id: Option<i64>,
    pub command_message_id: Option<i64>,
    pub command_channel_id: Option<i64>,
}

pub trait DraftMatchPort {
    fn persist_pending_match(
        &mut self,
        guild_id: i64,
        state: PendingMatchState,
    ) -> Result<i64, DraftError>;
    fn reserve_betting_seed(
        &mut self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<(), DraftError>;
    fn get_pending_match(
        &mut self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<Option<PendingMatchState>, DraftError>;
    fn store_message_info(
        &mut self,
        guild_id: i64,
        info: DraftMessageInfo,
    ) -> Result<(), DraftError>;
    fn schedule_betting_reminders(
        &mut self,
        guild_id: i64,
        pending_match_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<(), DraftError>;
}

pub trait DraftPublicationPort {
    fn resolve_channel(&mut self, channel_id: i64) -> bool;
    fn send(
        &mut self,
        channel_id: i64,
        payload: DraftMessagePayload,
    ) -> Result<PublishedDraftMessage, DraftError>;
    fn edit_partial(
        &mut self,
        channel_id: i64,
        message_id: i64,
        payload: DraftMessagePayload,
    ) -> Result<PublishedDraftMessage, DraftError>;
    fn delete_partial(&mut self, channel_id: i64, message_id: i64) -> Result<(), DraftError>;
    fn rename_thread(&mut self, thread_id: i64, name: &str) -> Result<(), DraftError>;
    fn lock_thread(&mut self, thread_id: i64) -> Result<(), DraftError>;
}

pub trait DraftInteractionPort {
    fn guild_id(&self) -> Option<i64>;
    fn user_id(&self) -> i64;
    fn user_display_name(&self) -> &str;
    fn channel_id(&self) -> i64;
    fn defer(&mut self) -> bool;
    fn followup(
        &mut self,
        payload: DraftMessagePayload,
        ephemeral: bool,
    ) -> Result<PublishedDraftMessage, DraftError>;
    fn delete_followup(&mut self, message_id: i64) -> Result<(), DraftError>;
    fn respond(&mut self, response: DraftResponse) -> Result<(), DraftError>;
    fn edit_original(
        &mut self,
        payload: DraftMessagePayload,
    ) -> Result<PublishedDraftMessage, DraftError>;
    fn original_message(&self) -> Option<PublishedDraftMessage>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecuteDraftRequest {
    pub guild_id: Option<i64>,
    pub lobby_kind: LobbyKind,
    pub regular_player_ids: Vec<i64>,
    pub conditional_player_ids: Vec<i64>,
    pub specified_captain1_id: Option<i64>,
    pub specified_captain2_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionContext {
    pub now: i64,
    pub bet_lock_seconds: i64,
    pub is_bomb_pot: bool,
}

impl Default for CompletionContext {
    fn default() -> Self {
        Self {
            now: 1_000_000,
            bet_lock_seconds: 300,
            is_bomb_pot: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionOutcome {
    pub pending_match_id: Option<i64>,
    pub response: Option<DraftResponse>,
    pub original_message: Option<PublishedDraftMessage>,
    pub lobby_message: Option<PublishedDraftMessage>,
    pub origin_message: Option<PublishedDraftMessage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DraftPickToken {
    pub guild_id: i64,
    pub session_id: u64,
    pub picker_id: i64,
    pub player_id: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftPickCommit {
    Picked { complete: bool },
    Rejected,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackedDraftView {
    pub id: u64,
    pub state_session_id: u64,
    pub finished: bool,
}

pub struct DraftApplication<P, L, M, D> {
    pub players: P,
    pub lobbies: L,
    pub matches: M,
    pub publication: D,
    pub state_manager: Arc<DraftStateManager>,
    pub service: DraftService,
    pub operation_log: Vec<String>,
    active_views: BTreeMap<i64, TrackedDraftView>,
    historical_views: BTreeMap<u64, TrackedDraftView>,
    next_view_id: u64,
}

impl<P, L, M, D> DraftApplication<P, L, M, D>
where
    P: DraftPlayerPort,
    L: DraftLobbyPort,
    M: DraftMatchPort,
    D: DraftPublicationPort,
{
    #[must_use]
    pub fn new(players: P, lobbies: L, matches: M, publication: D) -> Self {
        Self {
            players,
            lobbies,
            matches,
            publication,
            state_manager: Arc::new(DraftStateManager::default()),
            service: DraftService::new(),
            operation_log: Vec::new(),
            active_views: BTreeMap::new(),
            historical_views: BTreeMap::new(),
            next_view_id: 0,
        }
    }

    #[must_use]
    pub fn with_state_manager(mut self, state_manager: Arc<DraftStateManager>) -> Self {
        self.state_manager = state_manager;
        self
    }

    pub fn execute_draft<I: DraftInteractionPort>(
        &mut self,
        interaction: &mut I,
        request: &ExecuteDraftRequest,
    ) -> Result<bool, DraftError> {
        let guild_id = request.guild_id.unwrap_or(0);
        let all_player_ids = request
            .regular_player_ids
            .iter()
            .chain(&request.conditional_player_ids)
            .copied()
            .collect::<Vec<_>>();
        let players = self.players.get_by_ids(&all_player_ids, guild_id);
        let player_ratings = players
            .iter()
            .filter_map(|player| {
                player
                    .discord_id
                    .map(|player_id| (player_id, player.glicko_rating.unwrap_or(1500.0)))
            })
            .collect::<BTreeMap<_, _>>();
        let exclusion_counts = self.players.exclusion_counts(&all_player_ids, guild_id);
        let progress = interaction.followup(
            DraftMessagePayload {
                content: Some(format!(
                    "⚙️ {}: building the Immortal Draft player pool…",
                    request.lobby_kind.label()
                )),
                ..DraftMessagePayload::default()
            },
            false,
        )?;

        let mut captain_candidates = request.regular_player_ids.clone();
        for captain_id in [request.specified_captain1_id, request.specified_captain2_id]
            .into_iter()
            .flatten()
        {
            if !captain_candidates.contains(&captain_id) {
                captain_candidates.push(captain_id);
            }
        }
        let captain_pair = match self.service.select_captains(
            &captain_candidates,
            &player_ratings,
            request.specified_captain1_id,
            request.specified_captain2_id,
        ) {
            Ok(pair) => pair,
            Err(error) => {
                let _ = interaction.delete_followup(progress.id);
                interaction.respond(DraftResponse::ephemeral(format!("❌ {error}")))?;
                return Ok(false);
            }
        };
        let pool = match self.service.select_player_pool(
            &request.regular_player_ids,
            &request.conditional_player_ids,
            &exclusion_counts,
            &player_ratings,
            &[captain_pair.captain1_id, captain_pair.captain2_id],
            DRAFT_POOL_SIZE,
        ) {
            Ok(pool) => pool,
            Err(error) => {
                let _ = interaction.delete_followup(progress.id);
                interaction.respond(DraftResponse::ephemeral(format!("❌ {error}")))?;
                return Ok(false);
            }
        };
        if !self
            .lobbies
            .reserve_lobby_players(&pool.selected_ids, guild_id, request.lobby_kind)?
        {
            let _ = interaction.delete_followup(progress.id);
            interaction.respond(DraftResponse::ephemeral(
                "❌ The lobby changed while the draft was starting. Please try again.",
            ))?;
            return Ok(false);
        }
        let state = match self
            .state_manager
            .create_draft(request.guild_id, request.lobby_kind)
        {
            Ok(state) => state,
            Err(error) => {
                self.lobbies.release_lobby_players(
                    &pool.selected_ids,
                    guild_id,
                    request.lobby_kind,
                )?;
                let _ = interaction.delete_followup(progress.id);
                interaction.respond(DraftResponse::ephemeral(format!("❌ {error}")))?;
                return Ok(false);
            }
        };

        let lobby_channel_id = self.lobbies.lobby_channel_id(guild_id, request.lobby_kind);
        let origin_channel_id = self.lobbies.origin_channel_id(guild_id, request.lobby_kind);
        let draft_channel_id = lobby_channel_id
            .filter(|channel_id| self.publication.resolve_channel(*channel_id))
            .or_else(|| {
                origin_channel_id.filter(|channel_id| self.publication.resolve_channel(*channel_id))
            })
            .unwrap_or_else(|| interaction.channel_id());

        let full_exclusions = pool
            .excluded_ids
            .iter()
            .copied()
            .filter(|player_id| request.regular_player_ids.contains(player_id))
            .collect::<Vec<_>>();
        let half_exclusions = pool
            .excluded_ids
            .iter()
            .copied()
            .filter(|player_id| request.conditional_player_ids.contains(player_id))
            .collect::<Vec<_>>();
        let coinflip_winner = self
            .service
            .coinflip(captain_pair.captain1_id, captain_pair.captain2_id);
        {
            let mut state = lock_recover(&state);
            state.player_pool_ids.clone_from(&pool.selected_ids);
            state.excluded_player_ids.clone_from(&full_exclusions);
            state
                .full_exclusion_increment_ids
                .clone_from(&full_exclusions);
            state
                .half_exclusion_increment_ids
                .clone_from(&half_exclusions);
            state.captain1_id = Some(captain_pair.captain1_id);
            state.captain2_id = Some(captain_pair.captain2_id);
            state.captain1_rating = captain_pair.captain1_rating;
            state.captain2_rating = captain_pair.captain2_rating;
            state.coinflip_winner_id = Some(coinflip_winner);
            state.phase = DraftPhase::WinnerChoice;
            state.draft_channel_id = Some(draft_channel_id);
            state.player_pool_data = players
                .iter()
                .filter_map(|player| {
                    let player_id = player.discord_id?;
                    pool.selected_ids.contains(&player_id).then(|| {
                        (
                            player_id,
                            PlayerPoolEntry {
                                name: player.name.clone(),
                                rating: player.glicko_rating.unwrap_or(1500.0),
                                roles: player.preferred_roles.clone().unwrap_or_default(),
                            },
                        )
                    })
                })
                .collect();
        }
        let session_id = lock_recover(&state).session_id;
        self.track_draft_view(guild_id, session_id);
        let _ = interaction.delete_followup(progress.id);

        let ping_payload = DraftMessagePayload {
            content: Some(format!(
                "<@{}> <@{}> {} draft starting!",
                captain_pair.captain1_id,
                captain_pair.captain2_id,
                request.lobby_kind.label()
            )),
            ..DraftMessagePayload::default()
        };
        let ping = self.publication.send(draft_channel_id, ping_payload).ok();
        if let Some(ping) = &ping {
            lock_recover(&state).captain_ping_message_id = Some(ping.id);
        }

        let opening_embed =
            self.build_opening_embed(&lock_recover(&state), interaction.user_display_name());
        let opening_view = winner_choice_view(coinflip_winner, session_id);
        let message = match self.publication.send(
            draft_channel_id,
            DraftMessagePayload {
                embed: Some(opening_embed),
                view: Some(opening_view),
                ..DraftMessagePayload::default()
            },
        ) {
            Ok(message) => message,
            Err(error) => {
                self.stop_tracked_draft_view(guild_id, Some(session_id));
                self.state_manager
                    .clear_state(request.guild_id, Some(&state));
                self.lobbies.release_lobby_players(
                    &pool.selected_ids,
                    guild_id,
                    request.lobby_kind,
                )?;
                if let Some(ping) = ping {
                    let _ = self.publication.delete_partial(ping.channel_id, ping.id);
                }
                let _ = interaction.delete_followup(progress.id);
                return Err(error);
            }
        };
        lock_recover(&state).draft_message_id = Some(message.id);
        Ok(true)
    }

    fn build_opening_embed(&self, state: &DraftState, starter_name: &str) -> DraftEmbed {
        let captain1_name = state
            .captain1_id
            .and_then(|player_id| state.player_pool_data.get(&player_id))
            .map_or("Unknown", |player| player.name.as_str());
        let captain2_name = state
            .captain2_id
            .and_then(|player_id| state.player_pool_data.get(&player_id))
            .map_or("Unknown", |player| player.name.as_str());
        let winner_name = if state.coinflip_winner_id == state.captain1_id {
            captain1_name
        } else {
            captain2_name
        };
        DraftEmbed {
            title: format!("🎲 IMMORTAL DRAFT — {}", state.lobby_kind.label()),
            footer: Some(format!("Started by {starter_name}")),
            fields: vec![
                DraftEmbedField {
                    name: "👑 Captains".to_owned(),
                    value: format!(
                        "**{captain1_name}** ({:.0})\n**{captain2_name}** ({:.0})",
                        state.captain1_rating, state.captain2_rating
                    ),
                    inline: false,
                },
                DraftEmbedField {
                    name: "🎰 Coinflip Result".to_owned(),
                    value: format!("**{winner_name}** won the coinflip!"),
                    inline: false,
                },
                DraftEmbedField {
                    name: "📋 Player Pool".to_owned(),
                    value: build_player_pool_field(state),
                    inline: false,
                },
            ],
            ..DraftEmbed::default()
        }
    }

    #[must_use]
    pub fn build_live_draft_embed(&mut self, state: &DraftState) -> DraftEmbed {
        let current_captain_name = state
            .current_captain_id()
            .and_then(|player_id| state.player_pool_data.get(&player_id))
            .map_or_else(|| "N/A".to_owned(), |player| player.name.clone());
        let current_team = state.current_captain_team().unwrap_or("N/A");
        let round_number = (state.current_pick_index / 2 + 1).min(4);
        let pick_in_round = state.current_pick_index % 2 + 1;
        let first_is_radiant = state.current_round_first_captain_id == state.radiant_captain_id;
        let (first_team, second_team) = if first_is_radiant {
            ("🟢 Radiant", "🔴 Dire")
        } else {
            ("🔴 Dire", "🟢 Radiant")
        };
        DraftEmbed {
            title: format!(
                "⚔️ IMMORTAL DRAFT — {} - Player Selection",
                state.lobby_kind.label()
            ),
            description: format!(
                "**Round {round_number}/4:** {first_team} → {second_team}\n\
                 **Team Totals:** 🟢 {:.0} • 🔴 {:.0}\n\n\
                 **{current_captain_name}** ({}) to pick! (Pick {pick_in_round}/2)",
                state.radiant_rating_total(),
                state.dire_rating_total(),
                title_case(current_team)
            ),
            footer: Some(format!(
                "Pick #{}/{} | Click a player name to pick",
                (state.current_pick_index + 1).min(DRAFT_TOTAL_PICKS),
                DRAFT_TOTAL_PICKS
            )),
            ..DraftEmbed::default()
        }
    }

    pub fn complete_pre_draft_choices(&mut self, state: &mut DraftState) {
        let hero_pick_choice = if state.winner_choice_type.as_deref() == Some("side") {
            state.loser_choice_value.as_deref()
        } else {
            state.winner_choice_value.as_deref()
        };
        let hero_order_chooser = if state.winner_choice_type.as_deref() == Some("hero_pick") {
            state.coinflip_winner_id
        } else if state.coinflip_winner_id == state.captain1_id {
            state.captain2_id
        } else {
            state.captain1_id
        };
        let chooser_is_radiant = hero_order_chooser == state.radiant_captain_id;
        let chooser_first = hero_pick_choice == Some("first");
        state.radiant_hero_pick_order = Some(if chooser_is_radiant == chooser_first {
            1
        } else {
            2
        });
        state.dire_hero_pick_order = Some(if chooser_is_radiant == chooser_first {
            2
        } else {
            1
        });
        state.start_player_draft();
    }

    pub fn winner_chose_side(
        &mut self,
        guild_id: i64,
        expected_session_id: u64,
    ) -> Result<bool, DraftResponse> {
        let Some(state) = self.state_manager.get_state(Some(guild_id)) else {
            return Err(stale_draft_response());
        };
        let mut state = lock_recover(&state);
        if state.session_id != expected_session_id {
            return Err(stale_draft_response());
        }
        if state.phase != DraftPhase::WinnerChoice {
            return Err(DraftResponse::ephemeral(
                "❌ That choice has already been made.",
            ));
        }
        state.captain_ping_message_id = None;
        state.winner_choice_type = Some("side".to_owned());
        state.phase = DraftPhase::WinnerSideChoice;
        Ok(true)
    }

    pub fn final_hero_pick_choice(
        &mut self,
        guild_id: i64,
        expected_session_id: u64,
        pick_order: &str,
    ) -> Result<bool, DraftResponse> {
        let Some(state) = self.state_manager.get_state(Some(guild_id)) else {
            return Err(stale_draft_response());
        };
        let mut state = lock_recover(&state);
        if state.session_id != expected_session_id {
            return Err(stale_draft_response());
        }
        if state.phase != DraftPhase::LoserChoice {
            return Err(DraftResponse::ephemeral(
                "❌ That choice has already been made.",
            ));
        }
        state.loser_choice_value = Some(pick_order.to_owned());
        self.complete_pre_draft_choices(&mut state);
        Ok(true)
    }

    pub fn prepare_player_pick(
        &self,
        guild_id: i64,
        player_id: i64,
        picker_id: i64,
        expected_session_id: Option<u64>,
    ) -> Result<DraftPickToken, DraftResponse> {
        let Some(state) = self.state_manager.get_state(Some(guild_id)) else {
            return Err(stale_draft_response());
        };
        let state = lock_recover(&state);
        if expected_session_id.is_some_and(|expected| expected != state.session_id) {
            return Err(stale_draft_response());
        }
        if state.phase != DraftPhase::Drafting {
            return Err(DraftResponse::ephemeral(
                "❌ Draft is not in picking phase.",
            ));
        }
        if state.current_captain_id() != Some(picker_id) {
            return Err(DraftResponse::ephemeral("❌ It's not your turn!"));
        }
        if !state.available_player_ids().contains(&player_id) {
            return Err(DraftResponse::ephemeral(
                "❌ Cannot pick that player. They may already be picked.",
            ));
        }
        Ok(DraftPickToken {
            guild_id,
            session_id: state.session_id,
            picker_id,
            player_id,
        })
    }

    #[must_use]
    pub fn commit_player_pick(&self, token: DraftPickToken) -> DraftPickCommit {
        let Some(state) = self.state_manager.get_state(Some(token.guild_id)) else {
            return DraftPickCommit::Stale;
        };
        let mut state = lock_recover(&state);
        if state.session_id != token.session_id {
            return DraftPickCommit::Stale;
        }
        if !state.pick_player(token.player_id, Some(token.picker_id)) {
            return DraftPickCommit::Rejected;
        }
        DraftPickCommit::Picked {
            complete: state.phase == DraftPhase::Complete,
        }
    }

    pub fn commit_player_pick_with_feedback<I: DraftInteractionPort>(
        &self,
        interaction: &mut I,
        token: DraftPickToken,
    ) -> Result<DraftPickCommit, DraftError> {
        let result = self.commit_player_pick(token);
        let response = match result {
            DraftPickCommit::Rejected => Some(DraftResponse::ephemeral(
                "❌ Cannot pick that player. They may already be picked.",
            )),
            DraftPickCommit::Stale => Some(stale_draft_response()),
            DraftPickCommit::Picked { .. } => None,
        };
        if let Some(response) = response {
            interaction.followup(
                DraftMessagePayload {
                    content: Some(response.content),
                    ..DraftMessagePayload::default()
                },
                response.ephemeral,
            )?;
        }
        Ok(result)
    }

    pub fn update_draft_message(
        &mut self,
        channel_id: i64,
        message_id: i64,
        embed: DraftEmbed,
    ) -> Result<PublishedDraftMessage, DraftError> {
        self.operation_log.push("draft_partial".to_owned());
        self.publication.edit_partial(
            channel_id,
            message_id,
            DraftMessagePayload {
                embed: Some(embed),
                view: None,
                ..DraftMessagePayload::default()
            },
        )
    }

    pub fn release_draft_players(&mut self, state: &DraftState) -> Result<(), DraftError> {
        if state.player_pool_ids.is_empty() {
            return Ok(());
        }
        self.lobbies
            .release_lobby_players(&state.player_pool_ids, state.guild_id, state.lobby_kind)
    }

    pub fn restart_draft<I: DraftInteractionPort>(
        &mut self,
        interaction: &mut I,
        is_admin: bool,
    ) -> Result<bool, DraftError> {
        let guild_id = interaction.guild_id().unwrap_or(0);
        let Some(state) = self.state_manager.get_state(Some(guild_id)) else {
            interaction.respond(DraftResponse::ephemeral("❌ No active draft to restart."))?;
            return Ok(false);
        };
        let snapshot = lock_recover(&state).clone();
        if !is_admin
            && ![snapshot.captain1_id, snapshot.captain2_id]
                .into_iter()
                .flatten()
                .any(|captain_id| captain_id == interaction.user_id())
        {
            interaction.respond(DraftResponse::ephemeral(
                "❌ Only captains or server admins can restart the draft.",
            ))?;
            return Ok(false);
        }
        if snapshot.phase == DraftPhase::Complete {
            interaction.respond(DraftResponse::ephemeral(
                "⏳ Draft results are still being finalized. Please wait.",
            ))?;
            return Ok(false);
        }
        self.stop_tracked_draft_view(guild_id, Some(snapshot.session_id));
        self.state_manager.clear_state(Some(guild_id), Some(&state));
        self.release_draft_players(&snapshot)?;
        interaction.respond(DraftResponse::public(format!(
            "🔄 **Draft Restarted** by {}\n\n\
             The lobby has been preserved. Use `/draft start` to start a new draft.",
            interaction.user_display_name()
        )))?;
        self.operation_log.push("response".to_owned());
        if let (Some(channel_id), Some(message_id)) =
            (snapshot.draft_channel_id, snapshot.captain_ping_message_id)
        {
            self.operation_log.push("ping_partial".to_owned());
            let _ = self.publication.delete_partial(channel_id, message_id);
        }
        Ok(true)
    }

    pub fn track_draft_view(&mut self, guild_id: i64, state_session_id: u64) -> u64 {
        if let Some(previous) = self.active_views.get_mut(&guild_id) {
            previous.finished = true;
            self.historical_views.insert(previous.id, *previous);
        }
        self.next_view_id += 1;
        let view = TrackedDraftView {
            id: self.next_view_id,
            state_session_id,
            finished: false,
        };
        self.active_views.insert(guild_id, view);
        self.historical_views.insert(view.id, view);
        view.id
    }

    pub fn stop_tracked_draft_view(&mut self, guild_id: i64, expected_session_id: Option<u64>) {
        if self.active_views.get(&guild_id).is_some_and(|view| {
            expected_session_id.is_some_and(|expected| expected != view.state_session_id)
        }) {
            return;
        }
        if let Some(mut view) = self.active_views.remove(&guild_id) {
            view.finished = true;
            self.historical_views.insert(view.id, view);
        }
    }

    #[must_use]
    pub fn view_is_finished(&self, view_id: u64) -> bool {
        self.historical_views
            .get(&view_id)
            .is_some_and(|view| view.finished)
    }

    pub fn handle_timeout(
        &mut self,
        guild_id: i64,
        view_id: Option<u64>,
        view_state_session_id: Option<u64>,
    ) -> Result<bool, DraftError> {
        let Some(state) = self.state_manager.get_state(Some(guild_id)) else {
            return Ok(false);
        };
        let snapshot = lock_recover(&state).clone();
        if view_id.is_some()
            && (view_state_session_id != Some(snapshot.session_id)
                || self
                    .active_views
                    .get(&guild_id)
                    .is_some_and(|tracked| Some(tracked.id) != view_id))
        {
            return Ok(false);
        }
        if let (Some(channel_id), Some(message_id)) =
            (snapshot.draft_channel_id, snapshot.captain_ping_message_id)
        {
            self.operation_log.push("ping_partial".to_owned());
            let _ = self.publication.delete_partial(channel_id, message_id);
        }
        self.stop_tracked_draft_view(guild_id, Some(snapshot.session_id));
        self.state_manager.clear_state(Some(guild_id), Some(&state));
        self.release_draft_players(&snapshot)?;
        if let (Some(channel_id), Some(message_id)) =
            (snapshot.draft_channel_id, snapshot.draft_message_id)
        {
            self.operation_log.push("draft_partial".to_owned());
            let _ = self.publication.edit_partial(
                channel_id,
                message_id,
                DraftMessagePayload {
                    embed: Some(timeout_embed()),
                    view: None,
                    ..DraftMessagePayload::default()
                },
            );
        }
        Ok(true)
    }

    pub fn create_pending_match(
        &mut self,
        state: &DraftState,
        context: CompletionContext,
        thread_id: Option<i64>,
        origin_channel_id: Option<i64>,
    ) -> Result<(i64, PendingMatchState), DraftError> {
        let radiant_players = self
            .players
            .get_by_ids(&state.radiant_player_ids, state.guild_id);
        let dire_players = self
            .players
            .get_by_ids(&state.dire_player_ids, state.guild_id);
        let radiant_value = radiant_players
            .iter()
            .map(|player| player.glicko_rating.unwrap_or(1500.0))
            .sum::<f64>();
        let dire_value = dire_players
            .iter()
            .map(|player| player.glicko_rating.unwrap_or(1500.0))
            .sum::<f64>();
        let mut pending = PendingMatchState {
            lobby_kind: state.lobby_kind.value().to_owned(),
            radiant_team_ids: state.radiant_player_ids.clone(),
            dire_team_ids: state.dire_player_ids.clone(),
            excluded_player_ids: state.excluded_player_ids.clone(),
            excluded_conditional_player_ids: state.half_exclusion_increment_ids.clone(),
            exclusion_updates_deferred: true,
            full_exclusion_increment_ids: state.full_exclusion_increment_ids.clone(),
            half_exclusion_increment_ids: state.half_exclusion_increment_ids.clone(),
            radiant_value,
            dire_value,
            value_diff: (radiant_value - dire_value).abs(),
            first_pick_team: if state.radiant_hero_pick_order == Some(1) {
                "Radiant".to_owned()
            } else {
                "Dire".to_owned()
            },
            shuffle_timestamp: context.now,
            bet_lock_until: context.now + context.bet_lock_seconds,
            shuffle_message_id: state.draft_message_id,
            shuffle_channel_id: state.draft_channel_id,
            origin_channel_id: origin_channel_id.or(state.draft_channel_id),
            thread_shuffle_thread_id: thread_id,
            is_draft: true,
            is_bomb_pot: context.is_bomb_pot,
            ..PendingMatchState::default()
        };
        let pending_match_id = self
            .matches
            .persist_pending_match(state.guild_id, pending.clone())?;
        pending.pending_match_id = Some(pending_match_id);
        self.matches
            .reserve_betting_seed(state.guild_id, pending_match_id)?;
        Ok((pending_match_id, pending))
    }

    pub fn build_complete_embed(
        &mut self,
        state: &DraftState,
        pending: &PendingMatchState,
    ) -> DraftEmbed {
        let first_team = if state.radiant_hero_pick_order == Some(1) {
            "Radiant"
        } else {
            "Dire"
        };
        let pending_label = pending
            .pending_match_id
            .map_or_else(String::new, |id| format!(" — Match #{id}"));
        DraftEmbed {
            title: format!(
                "⚔️ IMMORTAL DRAFT — {}{} - Complete!",
                state.lobby_kind.label(),
                pending_label
            ),
            description: format!("**{first_team}** picks first in hero draft"),
            footer: Some("Use /record to record the match result when finished.".to_owned()),
            ..DraftEmbed::default()
        }
    }

    pub fn complete_draft<I: DraftInteractionPort>(
        &mut self,
        interaction: &mut I,
        state_handle: &DraftHandle,
        context: CompletionContext,
    ) -> CompletionOutcome {
        let state = lock_recover(state_handle).clone();
        let guild_id = state.guild_id;
        let kind = state.lobby_kind;
        let lobby_channel_id = self.lobbies.lobby_channel_id(guild_id, kind);
        let origin_channel_id = self.lobbies.origin_channel_id(guild_id, kind);
        let thread_id = self.lobbies.lobby_thread_id(guild_id, kind);
        let mut outcome = CompletionOutcome::default();

        let pending_creation =
            self.create_pending_match(&state, context, thread_id, origin_channel_id);
        let (pending_match_id, mut persisted_pending) = match pending_creation {
            Ok(value) => value,
            Err(_) => {
                let response = completion_error_guidance(None);
                let _ = interaction.respond(response.clone());
                outcome.response = Some(response);
                self.finish_completion(state_handle, &state, false);
                return outcome;
            }
        };
        outcome.pending_match_id = Some(pending_match_id);

        let drafted_ids = state
            .radiant_player_ids
            .iter()
            .chain(&state.dire_player_ids)
            .copied()
            .collect::<Vec<_>>();
        if let Err(error) = self
            .lobbies
            .evict_from_other_lobbies(&drafted_ids, guild_id, kind)
        {
            self.operation_log
                .push(format!("eviction warning: {error}"));
        }
        let pending = match self.matches.get_pending_match(guild_id, pending_match_id) {
            Ok(Some(pending)) => pending,
            Ok(None) => persisted_pending.clone(),
            Err(_) => {
                let response = completion_error_guidance(Some(pending_match_id));
                let _ = interaction.respond(response.clone());
                outcome.response = Some(response);
                self.finish_completion(state_handle, &state, true);
                return outcome;
            }
        };
        persisted_pending = pending;

        if self.lobbies.reset_lobby(guild_id, kind).is_err() {
            let response = completion_error_guidance(Some(pending_match_id));
            let _ = interaction.respond(response.clone());
            outcome.response = Some(response);
            self.finish_completion(state_handle, &state, true);
            return outcome;
        }
        self.operation_log.push("reset".to_owned());
        let _ = self.lobbies.clear_rally_cooldown(guild_id, kind);
        let embed = self.build_complete_embed(&state, &persisted_pending);
        let original_message = match interaction.edit_original(DraftMessagePayload {
            embed: Some(embed.clone()),
            view: None,
            ..DraftMessagePayload::default()
        }) {
            Ok(message) => message,
            Err(_) => {
                let response = completion_error_guidance(Some(pending_match_id));
                let _ = interaction.respond(response.clone());
                outcome.response = Some(response);
                self.finish_completion(state_handle, &state, true);
                return outcome;
            }
        };
        outcome.original_message = Some(original_message.clone());

        let mut messages_by_channel = BTreeMap::new();
        for channel_id in deduplicated(
            [lobby_channel_id, origin_channel_id]
                .into_iter()
                .flatten()
                .filter(|channel_id| *channel_id != original_message.channel_id),
        ) {
            if self.publication.resolve_channel(channel_id)
                && let Ok(message) = self.publication.send(
                    channel_id,
                    DraftMessagePayload {
                        embed: Some(embed.clone()),
                        ..DraftMessagePayload::default()
                    },
                )
            {
                messages_by_channel.insert(channel_id, message);
            }
        }
        outcome.lobby_message = lobby_channel_id.and_then(|channel_id| {
            if channel_id == original_message.channel_id {
                Some(original_message.clone())
            } else {
                messages_by_channel.get(&channel_id).cloned()
            }
        });
        outcome.origin_message = origin_channel_id.and_then(|channel_id| {
            if channel_id == original_message.channel_id {
                Some(original_message.clone())
            } else if Some(channel_id) == lobby_channel_id {
                outcome.lobby_message.clone()
            } else {
                messages_by_channel.get(&channel_id).cloned()
            }
        });

        let primary = outcome
            .lobby_message
            .clone()
            .unwrap_or_else(|| original_message.clone());
        let command = outcome
            .origin_message
            .clone()
            .filter(|origin| origin.id != primary.id);
        let message_info = DraftMessageInfo {
            message_id: Some(primary.id),
            channel_id: Some(primary.channel_id),
            jump_url: primary.jump_url.clone(),
            origin_channel_id: origin_channel_id.or(state.draft_channel_id),
            pending_match_id: Some(pending_match_id),
            command_message_id: command.as_ref().map(|message| message.id),
            command_channel_id: command.as_ref().map(|message| message.channel_id),
            ..DraftMessageInfo::default()
        };
        let _ = self.matches.store_message_info(guild_id, message_info);
        let _ = self
            .matches
            .schedule_betting_reminders(guild_id, pending_match_id, kind);

        if let Some(thread_id) = thread_id
            && self.publication.resolve_channel(thread_id)
        {
            let _ = self.publication.rename_thread(
                thread_id,
                &format!("🔒 {} Draft Complete - Awaiting Results", kind.label()),
            );
            if let Ok(thread_message) = self.publication.send(
                thread_id,
                DraftMessagePayload {
                    embed: Some(embed),
                    ..DraftMessagePayload::default()
                },
            ) {
                let _ = self.matches.store_message_info(
                    guild_id,
                    DraftMessageInfo {
                        thread_message_id: Some(thread_message.id),
                        pending_match_id: Some(pending_match_id),
                        ..DraftMessageInfo::default()
                    },
                );
            }
            let _ = self.publication.lock_thread(thread_id);
        }
        self.finish_completion(state_handle, &state, true);
        outcome
    }

    fn finish_completion(
        &mut self,
        state_handle: &DraftHandle,
        state: &DraftState,
        pending_created: bool,
    ) {
        self.stop_tracked_draft_view(state.guild_id, Some(state.session_id));
        self.state_manager
            .clear_state(Some(state.guild_id), Some(state_handle));
        let _ = self.release_draft_players(state);
        if pending_created {
            let _ = self.lobbies.refresh_bonus_pool_lobbies(state.guild_id);
            self.operation_log.push("refresh".to_owned());
        }
    }
}

#[must_use]
pub fn winner_choice_view(winner_id: i64, state_session_id: u64) -> DraftViewModel {
    DraftViewModel {
        timeout_seconds: PRE_DRAFT_TIMEOUT_SECONDS,
        current_captain_id: Some(winner_id),
        participant_ids: BTreeSet::from([winner_id]),
        buttons: vec![
            DraftButtonModel {
                label: "Choose Side".to_owned(),
                custom_id: "draft_choice_side".to_owned(),
                emoji: Some("🗺️".to_owned()),
                style: DraftButtonStyle::Primary,
                row: 0,
            },
            DraftButtonModel {
                label: "Choose Hero Pick Order".to_owned(),
                custom_id: "draft_choice_hero_pick".to_owned(),
                emoji: Some("⚔️".to_owned()),
                style: DraftButtonStyle::Primary,
                row: 0,
            },
        ],
        state_session_id: Some(state_session_id),
    }
}

#[must_use]
pub fn timeout_embed() -> DraftEmbed {
    DraftEmbed {
        title: "⏰ Draft Timed Out".to_owned(),
        description: concat!(
            "The draft was automatically cancelled due to inactivity.\n\n",
            "The lobby has been preserved. Use `/draft start` to begin a new draft."
        )
        .to_owned(),
        ..DraftEmbed::default()
    }
}

#[must_use]
pub fn completion_error_guidance(pending_match_id: Option<i64>) -> DraftResponse {
    match pending_match_id {
        Some(pending_match_id) => DraftResponse::ephemeral(format!(
            "⚠️ Draft complete — Match #{pending_match_id} was created, but an error occurred \
             while announcing it. Betting and recording still work: play the match and use \
             `/record`, or use `/record abort` to cancel it."
        )),
        None => DraftResponse::ephemeral(
            "⚠️ Draft complete but encountered an error during match setup. \
             Use `/draft start` to try again.",
        ),
    }
}

fn stale_draft_response() -> DraftResponse {
    DraftResponse::ephemeral(
        "❌ This draft is no longer active — use the controls on the current draft.",
    )
}

fn title_case(value: &str) -> String {
    let mut characters = value.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + characters.as_str()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::NamedTempFile;

    const TEST_GUILD_ID: i64 = 123;

    fn ratings(values: &[(i64, f64)]) -> BTreeMap<i64, f64> {
        values.iter().copied().collect()
    }

    fn cached(name: &str, rating: f64, roles: &[&str]) -> PlayerPoolEntry {
        PlayerPoolEntry {
            name: name.to_owned(),
            rating,
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
        }
    }

    fn draft_player(player_id: i64, rating: f64) -> Player {
        Player {
            name: format!("DraftPlayer{player_id}"),
            discord_id: Some(player_id),
            guild_id: Some(TEST_GUILD_ID),
            glicko_rating: Some(rating),
            preferred_roles: Some(vec!["1".to_owned()]),
            ..Player::default()
        }
    }

    fn dynamic_state(guild_id: i64, radiant_rating: f64, dire_rating: f64) -> DraftState {
        let mut state = DraftState::new(guild_id);
        state.captain1_id = Some(1);
        state.captain2_id = Some(2);
        state.captain1_rating = radiant_rating;
        state.captain2_rating = dire_rating;
        state.radiant_captain_id = Some(1);
        state.dire_captain_id = Some(2);
        state.player_pool_ids = (1..=10).collect();
        state.player_pool_data.insert(
            1,
            PlayerPoolEntry {
                rating: radiant_rating,
                ..PlayerPoolEntry::default()
            },
        );
        state.player_pool_data.insert(
            2,
            PlayerPoolEntry {
                rating: dire_rating,
                ..PlayerPoolEntry::default()
            },
        );
        for player_id in 3..=10 {
            state.player_pool_data.insert(
                player_id,
                PlayerPoolEntry {
                    rating: 1500.0,
                    ..PlayerPoolEntry::default()
                },
            );
        }
        state
    }

    #[test]
    fn test_captain_opt_in_command_is_removed() {
        assert!(!DRAFT_COMMANDS.contains(&"setcaptain"));
    }

    #[test]
    fn test_fake_player_commands_do_not_expose_captain_opt_in() {
        assert!(!ADMIN_FAKE_PLAYER_ARGUMENTS.contains(&"captain_eligible"));
    }

    #[test]
    fn test_initial_state() {
        let state = DraftState::new(123);
        assert_eq!(state.guild_id, 123);
        assert_eq!(state.phase, DraftPhase::Coinflip);
        assert!(state.player_pool_ids.is_empty());
        assert_eq!(state.captain1_id, None);
        assert_eq!(state.captain2_id, None);
        assert_eq!(state.current_pick_index, 0);
        assert_eq!(state.lobby_kind, LobbyKind::Open);
        assert!(state.player_pool_data.is_empty());
    }

    #[test]
    fn test_available_player_ids() {
        let mut state = DraftState::new(123);
        state.player_pool_ids = (1..=10).collect();
        state.radiant_player_ids = vec![1, 2];
        state.dire_player_ids = vec![3];
        let available = state.available_player_ids();
        assert_eq!(available, (4..=10).collect::<Vec<_>>());
    }

    #[test]
    fn test_available_player_ids_excludes_captains() {
        let mut state = DraftState::new(123);
        state.player_pool_ids = (1..=10).collect();
        state.radiant_captain_id = Some(1);
        state.dire_captain_id = Some(2);
        assert_eq!(state.available_player_ids(), (3..=10).collect::<Vec<_>>());
    }

    #[test]
    fn test_current_captain_id_during_drafting() {
        let mut state = DraftState::new(123);
        state.phase = DraftPhase::Drafting;
        state.radiant_captain_id = Some(100);
        state.dire_captain_id = Some(200);
        state.current_round_first_captain_id = Some(100);
        assert_eq!(state.current_captain_id(), Some(100));
        state.current_pick_index = 1;
        assert_eq!(state.current_captain_id(), Some(200));
    }

    #[test]
    fn test_current_captain_id_not_drafting() {
        assert_eq!(DraftState::new(123).current_captain_id(), None);
    }

    #[test]
    fn test_picks_remaining_this_turn() {
        let mut state = DraftState::new(123);
        state.phase = DraftPhase::Drafting;
        for pick_index in [0, 1, 3] {
            state.current_pick_index = pick_index;
            assert_eq!(state.picks_remaining_this_turn(), 1);
        }
    }

    #[test]
    fn test_pick_player_success() {
        let mut state = DraftState::new(123);
        state.phase = DraftPhase::Drafting;
        state.player_pool_ids = (1..=10).collect();
        state.radiant_captain_id = Some(100);
        state.dire_captain_id = Some(200);
        state.current_round_first_captain_id = Some(100);
        assert!(state.pick_player(5, None));
        assert!(state.radiant_player_ids.contains(&5));
        assert_eq!(state.current_pick_index, 1);
    }

    #[test]
    fn test_pick_player_invalid() {
        let mut state = DraftState::new(123);
        state.phase = DraftPhase::Drafting;
        state.player_pool_ids = vec![1, 2, 3];
        state.radiant_captain_id = Some(100);
        state.dire_captain_id = Some(200);
        state.current_round_first_captain_id = Some(100);
        assert!(!state.pick_player(999, None));
        assert_eq!(state.current_pick_index, 0);
    }

    #[test]
    fn test_pick_player_rejects_wrong_picker() {
        let mut state = DraftState::new(123);
        state.phase = DraftPhase::Drafting;
        state.player_pool_ids = (1..=10).collect();
        state.radiant_captain_id = Some(100);
        state.dire_captain_id = Some(200);
        state.current_round_first_captain_id = Some(100);
        assert!(!state.pick_player(5, Some(200)));
        assert_eq!(state.current_pick_index, 0);
        assert!(state.pick_player(5, Some(100)));
        assert_eq!(state.current_captain_id(), Some(200));
        assert!(!state.pick_player(6, Some(100)));
        assert_eq!(state.current_pick_index, 1);
    }

    #[test]
    fn test_draft_complete() {
        let mut state = DraftState::new(123);
        state.current_pick_index = 7;
        assert!(!state.is_draft_complete());
        state.current_pick_index = 8;
        assert!(state.is_draft_complete());
    }

    #[test]
    fn test_set_side_preference() {
        let mut state = DraftState::new(123);
        state.player_pool_ids = vec![1, 2, 3];
        assert!(state.set_side_preference(1, Some("radiant")));
        assert_eq!(
            state.side_preferences.get(&1).map(String::as_str),
            Some("radiant")
        );
        assert!(state.set_side_preference(1, None));
        assert!(!state.side_preferences.contains_key(&1));
    }

    #[test]
    fn test_to_dict_and_from_dict() {
        let mut state = DraftState::with_session(123, LobbyKind::LowSkill, 7);
        state.captain1_id = Some(100);
        state.captain2_id = Some(200);
        state.phase = DraftPhase::Drafting;
        state.player_pool_ids = vec![1, 2, 3];
        state.full_exclusion_increment_ids = vec![11, 12];
        state.half_exclusion_increment_ids = vec![13];
        let restored = DraftState::from_snapshot(state.to_snapshot());
        assert_eq!(restored, state);
    }

    #[test]
    fn authoritative_draft_state_round_trips_every_field_through_sqlite() {
        let fixture = NamedTempFile::new().expect("create draft persistence fixture");
        cama_db::schema_manager::initialize_or_migrate(fixture.path())
            .expect("initialize draft persistence fixture");
        let persistence = SqliteDraftStatePersistence::new(fixture.path());

        let mut state = DraftState::with_session(TEST_GUILD_ID, LobbyKind::LowSkill, 0);
        state.player_pool_ids = vec![11, 22, 33];
        state.player_pool_data = BTreeMap::from([
            (11, cached("Alice", 1812.5, &["1", "2"])),
            (22, cached("Bob", 1700.25, &["3"])),
            (33, cached("Carol", 1601.75, &[])),
        ]);
        state.excluded_player_ids = vec![44, 55];
        state.full_exclusion_increment_ids = vec![44];
        state.half_exclusion_increment_ids = vec![55];
        state.captain1_id = Some(11);
        state.captain2_id = Some(22);
        state.captain1_rating = 1812.5;
        state.captain2_rating = 1700.25;
        state.radiant_captain_id = Some(11);
        state.dire_captain_id = Some(22);
        state.coinflip_winner_id = Some(22);
        state.winner_choice_type = Some("side".to_owned());
        state.winner_choice_value = Some("radiant".to_owned());
        state.loser_choice_value = Some("first".to_owned());
        state.radiant_hero_pick_order = Some(1);
        state.dire_hero_pick_order = Some(2);
        state.current_round_first_captain_id = Some(11);
        state.current_pick_index = 5;
        state.radiant_player_ids = vec![11, 33];
        state.dire_player_ids = vec![22];
        state.side_preferences =
            BTreeMap::from([(33, "radiant".to_owned()), (44, "dire".to_owned())]);
        state.phase = DraftPhase::Drafting;
        state.draft_message_id = Some(9001);
        state.draft_channel_id = Some(9002);
        state.captain_ping_message_id = Some(9003);

        let mut expected = state.clone();
        let mut requested = state.to_snapshot().envelope(1);
        requested.deadline_at = Some(1_700_000_000);
        requested.active = true;
        requested.finalizing = true;
        requested.pending_match_id = Some(77);
        requested
            .application_metadata
            .insert("future_mode".to_owned(), json!("alpha"));
        requested.application_metadata.insert(
            "provider_context".to_owned(),
            json!({"attempt": 3, "flags": ["a", "b"]}),
        );
        let created = persistence
            .create_envelope(&requested)
            .expect("create full authoritative snapshot");
        expected.session_id = created.session_id;
        assert_eq!(created.revision, 1);
        assert_eq!(created.guild_id, TEST_GUILD_ID);
        assert_eq!(created.deadline_at, Some(1_700_000_000));
        assert!(created.active);
        assert!(created.finalizing);
        assert_eq!(created.pending_match_id, Some(77));
        assert_eq!(created.application_metadata["future_mode"], json!("alpha"));
        assert_eq!(
            created.application_metadata["provider_context"],
            json!({"attempt": 3, "flags": ["a", "b"]})
        );
        assert_eq!(created.state.as_state(), &expected);

        let loaded = persistence
            .load_all()
            .expect("load full authoritative snapshot");
        assert_eq!(loaded, vec![created.clone()]);
        assert_eq!(loaded[0].state.as_state(), &expected);

        let mut replacement = expected.clone();
        replacement.current_pick_index = 6;
        replacement.phase = DraftPhase::Complete;
        replacement.draft_message_id = Some(9010);
        let mut replacement_envelope = replacement.to_snapshot().envelope(2);
        replacement_envelope.deadline_at = Some(1_700_000_100);
        replacement_envelope.active = true;
        replacement_envelope.finalizing = false;
        replacement_envelope.pending_match_id = Some(88);
        replacement_envelope
            .application_metadata
            .insert("future_mode".to_owned(), json!("beta"));
        let replaced = persistence
            .replace_envelope_if_revision(
                TEST_GUILD_ID,
                created.session_id,
                created.revision,
                &replacement_envelope,
            )
            .expect("replace full authoritative snapshot");
        assert_eq!(replaced.revision, 2);
        assert_eq!(replaced.session_id, created.session_id);
        assert_eq!(replaced.deadline_at, Some(1_700_000_100));
        assert!(!replaced.finalizing);
        assert_eq!(replaced.pending_match_id, Some(88));
        assert_eq!(replaced.application_metadata["future_mode"], json!("beta"));
        assert_eq!(
            replaced.application_metadata["provider_context"],
            json!({"attempt": 3, "flags": ["a", "b"]})
        );
        assert_eq!(replaced.state.as_state(), &replacement);

        let mut bare_replacement = replacement.clone();
        bare_replacement.current_pick_index = 7;
        let preserved = persistence
            .replace_if_revision(
                TEST_GUILD_ID,
                replaced.session_id,
                replaced.revision,
                &bare_replacement.to_snapshot(),
            )
            .expect("replace snapshot while preserving recovery metadata");
        assert_eq!(preserved.deadline_at, Some(1_700_000_100));
        assert!(!preserved.finalizing);
        assert_eq!(preserved.pending_match_id, Some(88));
        assert_eq!(preserved.application_metadata["future_mode"], json!("beta"));
        assert_eq!(
            preserved.application_metadata["provider_context"],
            json!({"attempt": 3, "flags": ["a", "b"]})
        );
        assert_eq!(preserved.state.as_state(), &bare_replacement);
    }

    #[test]
    fn draft_envelope_rejects_mismatched_metadata_and_malformed_rows() {
        let state = DraftState::with_session(TEST_GUILD_ID, LobbyKind::Open, 9);
        let mut envelope = state.to_snapshot().envelope(1);
        envelope.deadline_at = Some(1_700_000_000);
        envelope.active = true;
        envelope.finalizing = true;
        envelope.pending_match_id = Some(77);
        let raw = envelope.to_json().expect("encode envelope");
        assert_eq!(
            DraftStateEnvelope::from_json(&raw).expect("round-trip envelope"),
            envelope
        );
        let mut value: serde_json::Value = serde_json::from_str(&raw).expect("decode envelope");
        value["guild_id"] = json!(999);
        assert!(matches!(
            DraftStateEnvelope::from_json(&value.to_string()),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));
        value["guild_id"] = json!(TEST_GUILD_ID);
        value["session_id"] = json!(10);
        assert!(matches!(
            DraftStateEnvelope::from_json(&value.to_string()),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));
        value["session_id"] = json!(9);
        value["revision"] = json!(0);
        assert!(matches!(
            DraftStateEnvelope::from_json(&value.to_string()),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));

        let fixture = NamedTempFile::new().expect("create malformed-row fixture");
        cama_db::schema_manager::initialize_or_migrate(fixture.path())
            .expect("initialize malformed-row fixture");
        Connection::open(fixture.path())
            .expect("open malformed-row fixture")
            .execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,?2,?3)",
                rusqlite::params![
                    TEST_GUILD_ID,
                    cama_db::draft_state::DRAFT_STATE_KEY,
                    value.to_string()
                ],
            )
            .expect("insert malformed metadata row");
        let persistence = SqliteDraftStatePersistence::new(fixture.path());
        assert!(matches!(
            persistence.load_all(),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));
    }

    #[test]
    fn test_pending_exclusion_update_metadata_survives_round_trip() {
        let state = PendingMatchState {
            lobby_kind: "lowskill".to_owned(),
            exclusion_updates_deferred: true,
            full_exclusion_increment_ids: vec![11, 12],
            half_exclusion_increment_ids: vec![13],
            ..PendingMatchState::default()
        };
        let restored = PendingMatchState::from_snapshot(state.to_snapshot());
        assert_eq!(restored.lobby_kind, "lowskill");
        assert!(restored.exclusion_updates_deferred);
        assert_eq!(restored.full_exclusion_increment_ids, [11, 12]);
        assert_eq!(restored.half_exclusion_increment_ids, [13]);
    }

    #[test]
    fn test_player_pool_data_serialization() {
        let mut state = DraftState::new(123);
        state.player_pool_ids = vec![1, 2, 3];
        state.player_pool_data = BTreeMap::from([
            (1, cached("Alice", 1800.5, &[])),
            (2, cached("Bob", 1650.0, &["3"])),
            (3, cached("Charlie", 1500.0, &["1", "2", "3", "4", "5"])),
        ]);
        let restored = DraftState::from_snapshot(state.to_snapshot());
        assert_eq!(restored.player_pool_data, state.player_pool_data);
        assert_eq!(restored.player_pool_data[&1].rating, 1800.5);
        assert!(restored.player_pool_data[&1].roles.is_empty());
        assert_eq!(restored.player_pool_data[&3].roles.len(), 5);
    }

    #[test]
    fn test_draft_state_lifecycle() {
        let manager = DraftStateManager::default();
        assert!(manager.get_state(Some(999)).is_none());
        let state = manager.create_draft(Some(123), LobbyKind::Open).unwrap();
        assert!(manager.has_active_draft(Some(123)));
        assert!(Arc::ptr_eq(&manager.get_state(Some(123)).unwrap(), &state));
        assert!(manager.create_draft(Some(123), LobbyKind::Open).is_err());
        lock_recover(&state).phase = DraftPhase::Drafting;
        assert!(manager.create_draft(Some(123), LobbyKind::Open).is_err());
        assert!(manager.clear_state(Some(123), None).is_some());
        assert!(!manager.has_active_draft(Some(123)));
        let replacement = manager.create_draft(Some(123), LobbyKind::Open).unwrap();
        assert!(!Arc::ptr_eq(&state, &replacement));
        assert_eq!(lock_recover(&replacement).phase, DraftPhase::Coinflip);
    }

    #[test]
    fn test_concurrent_create_draft_allows_exactly_one_winner() {
        let manager = Arc::new(DraftStateManager::default());
        let barrier = Arc::new(Barrier::new(2));
        let threads = (0..2)
            .map(|_| {
                let manager = Arc::clone(&manager);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    manager.create_draft(Some(123), LobbyKind::Open).is_ok()
                })
            })
            .collect::<Vec<_>>();
        let results = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| **result).count(), 1);
        assert_eq!(results.iter().filter(|result| !**result).count(), 1);
    }

    #[test]
    fn test_complete_draft_remains_reserved_until_cleanup() {
        let manager = DraftStateManager::default();
        let state = manager.create_draft(Some(123), LobbyKind::Open).unwrap();
        lock_recover(&state).phase = DraftPhase::Complete;
        assert!(manager.has_active_draft(Some(123)));
        assert!(manager.create_draft(Some(123), LobbyKind::Open).is_err());
    }

    #[test]
    fn test_identity_checked_cleanup_cannot_delete_newer_draft() {
        let manager = DraftStateManager::default();
        let old = manager.create_draft(Some(123), LobbyKind::Open).unwrap();
        assert!(manager.clear_state(Some(123), Some(&old)).is_some());
        let new = manager.create_draft(Some(123), LobbyKind::Open).unwrap();
        assert!(manager.clear_state(Some(123), Some(&old)).is_none());
        assert!(Arc::ptr_eq(&manager.get_state(Some(123)).unwrap(), &new));
    }

    #[test]
    fn test_advance_phase() {
        let manager = DraftStateManager::default();
        manager.create_draft(Some(123), LobbyKind::Open).unwrap();
        assert!(manager.advance_phase(Some(123), DraftPhase::WinnerChoice));
        assert_eq!(
            lock_recover(&manager.get_state(Some(123)).unwrap()).phase,
            DraftPhase::WinnerChoice
        );
    }

    #[test]
    fn test_guild_id_normalization() {
        let manager = DraftStateManager::default();
        let state = manager.create_draft(None, LobbyKind::Open).unwrap();
        assert!(Arc::ptr_eq(&manager.get_state(None).unwrap(), &state));
        assert!(Arc::ptr_eq(&manager.get_state(Some(0)).unwrap(), &state));
    }

    #[test]
    fn test_select_captains_both_specified() {
        let service = DraftService::new();
        let result = service
            .select_captains(
                &[100, 200, 300],
                &ratings(&[(100, 1500.0), (200, 1600.0)]),
                Some(100),
                Some(200),
            )
            .unwrap();
        assert_eq!(result.captain1_id, 100);
        assert_eq!(result.captain2_id, 200);
        assert_eq!(result.captain1_rating, 1500.0);
        assert_eq!(result.captain2_rating, 1600.0);
    }

    #[test]
    fn test_select_captains_not_enough_players() {
        let error = DraftService::new()
            .select_captains(&[100], &ratings(&[(100, 1500.0)]), None, None)
            .unwrap_err();
        assert!(error.message.contains("at least 2"));
    }

    #[test]
    fn test_select_captains_chooses_closest_glicko_pair_without_randomness() {
        let result = DraftService::new()
            .select_captains(
                &[100, 200, 300, 400],
                &ratings(&[(100, 1500.0), (200, 1795.0), (300, 1800.0), (400, 2200.0)]),
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            BTreeSet::from([result.captain1_id, result.captain2_id]),
            BTreeSet::from([200, 300])
        );
        assert_eq!((result.captain1_rating - result.captain2_rating).abs(), 5.0);
    }

    #[test]
    fn test_select_captains_with_specified_captain_chooses_closest_rating() {
        let result = DraftService::new()
            .select_captains(
                &[100, 200, 300],
                &ratings(&[(100, 1500.0), (200, 1500.0), (300, 2000.0)]),
                Some(100),
                None,
            )
            .unwrap();
        assert_eq!(result.captain2_id, 200);
    }

    #[test]
    fn test_select_player_pool_exact_size() {
        let result = DraftService::new()
            .select_player_pool(
                &(1..=10).collect::<Vec<_>>(),
                &[],
                &BTreeMap::new(),
                &BTreeMap::new(),
                &[],
                10,
            )
            .unwrap();
        assert_eq!(result.selected_ids.len(), 10);
        assert!(result.excluded_ids.is_empty());
    }

    #[test]
    fn test_select_player_pool_prioritizes_exclusion_factor_over_glicko() {
        let mut player_ratings = (1..=11)
            .map(|player_id| (player_id, 2100.0 - player_id as f64 * 50.0))
            .collect::<BTreeMap<_, _>>();
        player_ratings.insert(11, 900.0);
        let result = DraftService::new()
            .select_player_pool(
                &(1..=11).collect::<Vec<_>>(),
                &[],
                &BTreeMap::from([(11, 1)]),
                &player_ratings,
                &[],
                10,
            )
            .unwrap();
        assert!(result.selected_ids.contains(&11));
        assert!(result.excluded_ids.contains(&10));
    }

    #[test]
    fn test_select_player_pool_uses_glicko_for_equal_exclusion_factors() {
        let player_ratings = (1..=11)
            .map(|player_id| (player_id, 1000.0 + player_id as f64 * 50.0))
            .collect::<BTreeMap<_, _>>();
        let result = DraftService::new()
            .select_player_pool(
                &(1..=11).collect::<Vec<_>>(),
                &[],
                &BTreeMap::new(),
                &player_ratings,
                &[],
                10,
            )
            .unwrap();
        assert_eq!(result.excluded_ids, [1]);
        assert_eq!(
            result.selected_ids.into_iter().collect::<BTreeSet<_>>(),
            (2..=11).collect()
        );
    }

    #[test]
    fn test_select_player_pool_preserves_lobby_order_for_exact_ties() {
        let lobby_order = vec![9, 3, 7, 1, 8, 2, 6, 4, 5, 10, 11];
        let player_ratings = lobby_order
            .iter()
            .map(|player_id| (*player_id, 1500.0))
            .collect();
        let result = DraftService::new()
            .select_player_pool(
                &lobby_order,
                &[],
                &BTreeMap::new(),
                &player_ratings,
                &[],
                10,
            )
            .unwrap();
        assert_eq!(result.selected_ids, lobby_order[..10]);
        assert_eq!(result.excluded_ids, [11]);
    }

    #[test]
    fn test_select_player_pool_uses_conditionals_only_to_fill_shortage() {
        let result = DraftService::new()
            .select_player_pool(
                &(1..=8).collect::<Vec<_>>(),
                &[9, 10, 11, 12],
                &BTreeMap::from([(9, 1), (10, 3), (11, 2), (12, 99)]),
                &(1..=12).map(|id| (id, 1500.0)).collect(),
                &[],
                10,
            )
            .unwrap();
        assert!((1..=8).all(|id| result.selected_ids.contains(&id)));
        assert!(result.selected_ids.contains(&12));
        assert!(result.selected_ids.contains(&10));
        assert_eq!(
            result.excluded_ids.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([9, 11])
        );
    }

    #[test]
    fn test_select_player_pool_forces_conditional_captain_into_full_regular_pool() {
        let result = DraftService::new()
            .select_player_pool(
                &(1..=11).collect::<Vec<_>>(),
                &[12],
                &BTreeMap::new(),
                &(1..=12).map(|id| (id, 2200.0 - id as f64 * 50.0)).collect(),
                &[12],
                10,
            )
            .unwrap();
        assert!(result.selected_ids.contains(&12));
        assert!(result.excluded_ids.contains(&10));
        assert!(result.excluded_ids.contains(&11));
    }

    #[test]
    fn test_select_player_pool_not_enough() {
        let error = DraftService::new()
            .select_player_pool(&[1, 2, 3], &[], &BTreeMap::new(), &BTreeMap::new(), &[], 10)
            .unwrap_err();
        assert!(error.message.contains("Need at least"));
    }

    #[test]
    fn test_coinflip() {
        let mut service = DraftService::new();
        let results = (0..100)
            .map(|_| service.coinflip(100, 200))
            .collect::<BTreeSet<_>>();
        assert_eq!(results, BTreeSet::from([100, 200]));
    }

    #[test]
    fn test_lower_total_team_picks_first_and_other_team_picks_second() {
        let mut state = dynamic_state(123, 1400.0, 1600.0);
        state.start_player_draft();
        assert_eq!(state.current_captain_id(), Some(1));
        assert!(state.pick_player(3, Some(1)));
        assert_eq!(state.current_captain_id(), Some(2));
    }

    #[test]
    fn test_team_totals_are_recalculated_before_each_round() {
        let mut state = dynamic_state(123, 1400.0, 1600.0);
        state.player_pool_data.get_mut(&3).unwrap().rating = 600.0;
        state.player_pool_data.get_mut(&4).unwrap().rating = 100.0;
        state.start_player_draft();
        assert!(state.pick_player(3, Some(1)));
        assert!(state.pick_player(4, Some(2)));
        assert_eq!(state.radiant_rating_total(), 2000.0);
        assert_eq!(state.dire_rating_total(), 1700.0);
        assert_eq!(state.current_captain_id(), Some(2));
    }

    #[test]
    fn test_tied_later_round_starts_with_previous_round_second_picker() {
        let mut state = dynamic_state(123, 1400.0, 1600.0);
        state.player_pool_data.get_mut(&3).unwrap().rating = 200.0;
        state.player_pool_data.get_mut(&4).unwrap().rating = 0.0;
        state.start_player_draft();
        assert!(state.pick_player(3, Some(1)));
        assert!(state.pick_player(4, Some(2)));
        assert_eq!(state.radiant_rating_total(), state.dire_rating_total());
        assert_eq!(state.current_captain_id(), Some(2));
    }

    #[test]
    fn test_tied_first_round_starts_with_coinflip_loser() {
        let mut state = dynamic_state(123, 1500.0, 1500.0);
        state.coinflip_winner_id = Some(1);
        state.start_player_draft();
        assert_eq!(state.current_captain_id(), Some(2));
    }

    fn full_dynamic_state(guild_id: i64) -> DraftState {
        let mut state = DraftState::new(guild_id);
        state.radiant_captain_id = Some(1);
        state.dire_captain_id = Some(2);
        state.captain1_id = Some(1);
        state.captain2_id = Some(2);
        state.captain1_rating = 1400.0;
        state.captain2_rating = 1500.0;
        state.player_pool_ids = vec![1, 2, 11, 12, 13, 14, 15, 16, 17, 18];
        state.player_pool_data.insert(1, cached("One", 1400.0, &[]));
        state.player_pool_data.insert(2, cached("Two", 1500.0, &[]));
        for player_id in 11..=18 {
            state
                .player_pool_data
                .insert(player_id, cached("Player", 1500.0, &[]));
        }
        state.start_player_draft();
        state
    }

    #[test]
    fn test_full_dynamic_draft_assigns_four_picks_per_side() {
        let mut state = full_dynamic_state(1001);
        let mut picker_log = Vec::new();
        for _ in 0..8 {
            let picker = state.current_captain_id().unwrap();
            picker_log.push(picker);
            let player_id = state.available_player_ids()[0];
            assert!(state.pick_player(player_id, Some(picker)));
        }
        assert_eq!(state.phase, DraftPhase::Complete);
        assert_eq!(state.current_pick_index, 8);
        assert_eq!(picker_log, [1, 2, 1, 2, 1, 2, 1, 2]);
        assert_eq!(state.radiant_player_ids.len(), 5);
        assert_eq!(state.dire_player_ids.len(), 5);
        assert!(state.available_player_ids().is_empty());
    }

    #[test]
    fn test_guild_isolation_draft_states() {
        let mut state_a = full_dynamic_state(2001);
        let mut state_b = full_dynamic_state(2002);
        state_b.player_pool_ids = vec![1, 2, 21, 22, 23, 24, 25, 26, 27, 28];
        for player_id in 21..=28 {
            state_b
                .player_pool_data
                .insert(player_id, cached("Player", 1500.0, &[]));
        }
        for _ in 0..8 {
            let player_id = state_a.available_player_ids()[0];
            assert!(state_a.pick_player(player_id, None));
        }
        assert_eq!(state_b.current_pick_index, 0);
        assert_eq!(state_b.available_player_ids().len(), 8);
        for _ in 0..8 {
            let player_id = state_b.available_player_ids()[0];
            assert!(state_b.pick_player(player_id, None));
        }
        let a_non_captains = state_a
            .radiant_player_ids
            .into_iter()
            .filter(|id| ![1, 2].contains(id))
            .collect::<BTreeSet<_>>();
        let b_non_captains = state_b
            .radiant_player_ids
            .into_iter()
            .filter(|id| ![1, 2].contains(id))
            .collect::<BTreeSet<_>>();
        assert!(a_non_captains.is_disjoint(&b_non_captains));
    }

    #[test]
    fn test_set_and_get_captain_eligible() {
        let mut repository = MemoryDraftPlayers::default();
        repository
            .add(TEST_GUILD_ID, draft_player(1001, 1500.0))
            .unwrap();
        assert!(!repository.get_captain_eligible(1001, TEST_GUILD_ID));
        assert!(repository.set_captain_eligible(1001, TEST_GUILD_ID, true));
        assert!(repository.get_captain_eligible(1001, TEST_GUILD_ID));
        assert!(repository.set_captain_eligible(1001, TEST_GUILD_ID, false));
        assert!(!repository.get_captain_eligible(1001, TEST_GUILD_ID));
        assert!(!repository.get_captain_eligible(9999, TEST_GUILD_ID));
        assert!(!repository.set_captain_eligible(9999, TEST_GUILD_ID, true));
    }

    #[test]
    fn test_get_captain_eligible_players() {
        let mut repository = MemoryDraftPlayers::default();
        assert!(
            repository
                .get_captain_eligible_players(&[], TEST_GUILD_ID)
                .is_empty()
        );
        for player_id in 2001..=2005 {
            repository
                .add(TEST_GUILD_ID, draft_player(player_id, 1500.0))
                .unwrap();
        }
        assert!(
            repository
                .get_captain_eligible_players(&[2001, 2002, 2003], TEST_GUILD_ID)
                .is_empty()
        );
        for player_id in [2001, 2003, 2005] {
            assert!(repository.set_captain_eligible(player_id, TEST_GUILD_ID, true));
        }
        assert_eq!(
            repository.get_captain_eligible_players(&[2001, 2002, 2003, 2004, 2005], TEST_GUILD_ID),
            [2001, 2003, 2005]
        );
        assert_eq!(
            repository.get_captain_eligible_players(&[2001, 2004], TEST_GUILD_ID),
            [2001]
        );
    }

    #[test]
    fn test_excludes_captains_sorts_by_rating_and_falls_back() {
        let mut state = DraftState::new(123);
        state.player_pool_ids = vec![1, 2, 3, 4, 5];
        state.captain1_id = Some(1);
        state.captain2_id = Some(2);
        state.player_pool_data = BTreeMap::from([
            (1, cached("Captain1", 1850.0, &["1"])),
            (2, cached("Captain2", 1820.0, &["2"])),
            (3, cached("LowRating", 1200.0, &["5"])),
            (4, cached("HighRating", 1900.0, &[])),
        ]);
        let display = build_player_pool_field(&state);
        let lines = display.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert!(!display.contains("Captain1"));
        assert!(!display.contains("Captain2"));
        assert!(lines[0].starts_with("HighRating (1900)"));
        assert!(lines[1].starts_with("Player 5 (1500)"));
        assert!(lines[2].starts_with("LowRating (1200)"));
    }

    #[test]
    fn test_empty_pool_when_all_players_are_captains() {
        let mut state = DraftState::new(123);
        state.player_pool_ids = vec![1, 2];
        state.captain1_id = Some(1);
        state.captain2_id = Some(2);
        assert_eq!(build_player_pool_field(&state), "No players in pool");
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct LobbyCall {
        player_ids: Vec<i64>,
        guild_id: i64,
        kind: LobbyKind,
    }

    #[derive(Clone, Debug)]
    struct RecordingLobby {
        reserve_result: bool,
        reserve_calls: Vec<LobbyCall>,
        release_calls: Vec<LobbyCall>,
        reset_calls: Vec<(i64, LobbyKind)>,
        lobby_channel_id: Option<i64>,
        origin_channel_id: Option<i64>,
        thread_id: Option<i64>,
        evict_calls: Vec<LobbyCall>,
        evict_error: Option<DraftError>,
        cooldown_clear_calls: Vec<(i64, LobbyKind)>,
        refresh_calls: Vec<i64>,
    }

    impl Default for RecordingLobby {
        fn default() -> Self {
            Self {
                reserve_result: true,
                reserve_calls: Vec::new(),
                release_calls: Vec::new(),
                reset_calls: Vec::new(),
                lobby_channel_id: None,
                origin_channel_id: None,
                thread_id: None,
                evict_calls: Vec::new(),
                evict_error: None,
                cooldown_clear_calls: Vec::new(),
                refresh_calls: Vec::new(),
            }
        }
    }

    impl DraftLobbyPort for RecordingLobby {
        fn reserve_lobby_players(
            &mut self,
            player_ids: &[i64],
            guild_id: i64,
            lobby_kind: LobbyKind,
        ) -> Result<bool, DraftError> {
            self.reserve_calls.push(LobbyCall {
                player_ids: player_ids.to_vec(),
                guild_id,
                kind: lobby_kind,
            });
            Ok(self.reserve_result)
        }

        fn release_lobby_players(
            &mut self,
            player_ids: &[i64],
            guild_id: i64,
            lobby_kind: LobbyKind,
        ) -> Result<(), DraftError> {
            self.release_calls.push(LobbyCall {
                player_ids: player_ids.to_vec(),
                guild_id,
                kind: lobby_kind,
            });
            Ok(())
        }

        fn reset_lobby(&mut self, guild_id: i64, lobby_kind: LobbyKind) -> Result<(), DraftError> {
            self.reset_calls.push((guild_id, lobby_kind));
            Ok(())
        }

        fn lobby_channel_id(&mut self, _guild_id: i64, _lobby_kind: LobbyKind) -> Option<i64> {
            self.lobby_channel_id
        }

        fn origin_channel_id(&mut self, _guild_id: i64, _lobby_kind: LobbyKind) -> Option<i64> {
            self.origin_channel_id
        }

        fn lobby_thread_id(&mut self, _guild_id: i64, _lobby_kind: LobbyKind) -> Option<i64> {
            self.thread_id
        }

        fn evict_from_other_lobbies(
            &mut self,
            player_ids: &[i64],
            guild_id: i64,
            popped_kind: LobbyKind,
        ) -> Result<(), DraftError> {
            self.evict_calls.push(LobbyCall {
                player_ids: player_ids.to_vec(),
                guild_id,
                kind: popped_kind,
            });
            self.evict_error.clone().map_or(Ok(()), Err)
        }

        fn clear_rally_cooldown(
            &mut self,
            guild_id: i64,
            lobby_kind: LobbyKind,
        ) -> Result<(), DraftError> {
            self.cooldown_clear_calls.push((guild_id, lobby_kind));
            Ok(())
        }

        fn refresh_bonus_pool_lobbies(&mut self, guild_id: i64) -> Result<(), DraftError> {
            self.refresh_calls.push(guild_id);
            Ok(())
        }
    }

    #[derive(Clone, Debug)]
    struct RecordingMatch {
        next_id: i64,
        pending: Option<PendingMatchState>,
        persist_error: Option<DraftError>,
        get_error: Option<DraftError>,
        reserve_calls: Vec<(i64, i64)>,
        message_info: Vec<DraftMessageInfo>,
        reminder_calls: Vec<(i64, i64, LobbyKind)>,
    }

    impl Default for RecordingMatch {
        fn default() -> Self {
            Self {
                next_id: 1234,
                pending: None,
                persist_error: None,
                get_error: None,
                reserve_calls: Vec::new(),
                message_info: Vec::new(),
                reminder_calls: Vec::new(),
            }
        }
    }

    impl DraftMatchPort for RecordingMatch {
        fn persist_pending_match(
            &mut self,
            _guild_id: i64,
            mut state: PendingMatchState,
        ) -> Result<i64, DraftError> {
            if let Some(error) = &self.persist_error {
                return Err(error.clone());
            }
            state.pending_match_id = Some(self.next_id);
            self.pending = Some(state);
            Ok(self.next_id)
        }

        fn reserve_betting_seed(
            &mut self,
            guild_id: i64,
            pending_match_id: i64,
        ) -> Result<(), DraftError> {
            self.reserve_calls.push((guild_id, pending_match_id));
            Ok(())
        }

        fn get_pending_match(
            &mut self,
            _guild_id: i64,
            pending_match_id: i64,
        ) -> Result<Option<PendingMatchState>, DraftError> {
            if let Some(error) = &self.get_error {
                return Err(error.clone());
            }
            Ok(self
                .pending
                .clone()
                .filter(|pending| pending.pending_match_id == Some(pending_match_id)))
        }

        fn store_message_info(
            &mut self,
            _guild_id: i64,
            info: DraftMessageInfo,
        ) -> Result<(), DraftError> {
            if let Some(pending) = self.pending.as_mut()
                && info.pending_match_id == pending.pending_match_id
            {
                if let Some(message_id) = info.message_id {
                    pending.shuffle_message_id = Some(message_id);
                }
                if let Some(channel_id) = info.channel_id {
                    pending.shuffle_channel_id = Some(channel_id);
                }
                if let Some(jump_url) = &info.jump_url {
                    pending.shuffle_message_jump_url = Some(jump_url.clone());
                }
                if let Some(origin_channel_id) = info.origin_channel_id {
                    pending.origin_channel_id = Some(origin_channel_id);
                }
                if let Some(thread_message_id) = info.thread_message_id {
                    pending.thread_shuffle_message_id = Some(thread_message_id);
                }
                if let Some(thread_id) = info.thread_id {
                    pending.thread_shuffle_thread_id = Some(thread_id);
                }
            }
            self.message_info.push(info);
            Ok(())
        }

        fn schedule_betting_reminders(
            &mut self,
            guild_id: i64,
            pending_match_id: i64,
            lobby_kind: LobbyKind,
        ) -> Result<(), DraftError> {
            self.reminder_calls
                .push((guild_id, pending_match_id, lobby_kind));
            Ok(())
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SentPublication {
        message: PublishedDraftMessage,
        payload: DraftMessagePayload,
    }

    #[derive(Clone, Debug, Default)]
    struct RecordingPublication {
        available_channels: BTreeSet<i64>,
        next_message_id: i64,
        send_attempts: Vec<(i64, DraftMessagePayload)>,
        sent: Vec<SentPublication>,
        partial_edits: Vec<(i64, i64, DraftMessagePayload)>,
        partial_deletes: Vec<(i64, i64)>,
        fail_ping_channels: BTreeSet<i64>,
        fail_embed_channels: BTreeSet<i64>,
        renamed_threads: Vec<(i64, String)>,
        locked_threads: Vec<i64>,
        resolve_calls: Vec<i64>,
    }

    impl RecordingPublication {
        fn channel_messages(&self, channel_id: i64) -> Vec<&SentPublication> {
            self.sent
                .iter()
                .filter(|sent| sent.message.channel_id == channel_id)
                .collect()
        }
    }

    impl DraftPublicationPort for RecordingPublication {
        fn resolve_channel(&mut self, channel_id: i64) -> bool {
            self.resolve_calls.push(channel_id);
            self.available_channels.contains(&channel_id)
        }

        fn send(
            &mut self,
            channel_id: i64,
            payload: DraftMessagePayload,
        ) -> Result<PublishedDraftMessage, DraftError> {
            self.send_attempts.push((channel_id, payload.clone()));
            if payload.content.is_some() && self.fail_ping_channels.contains(&channel_id) {
                return Err(DraftError::new("simulated captain ping failure"));
            }
            if payload.embed.is_some() && self.fail_embed_channels.contains(&channel_id) {
                return Err(DraftError::new("simulated Discord send failure"));
            }
            self.next_message_id += 1;
            let message = PublishedDraftMessage {
                id: 9_000_000 + self.next_message_id,
                channel_id,
                jump_url: Some(format!(
                    "https://discord.test/channels/0/{channel_id}/{}",
                    9_000_000 + self.next_message_id
                )),
            };
            self.sent.push(SentPublication {
                message: message.clone(),
                payload,
            });
            Ok(message)
        }

        fn edit_partial(
            &mut self,
            channel_id: i64,
            message_id: i64,
            payload: DraftMessagePayload,
        ) -> Result<PublishedDraftMessage, DraftError> {
            self.partial_edits.push((channel_id, message_id, payload));
            Ok(PublishedDraftMessage {
                id: message_id,
                channel_id,
                jump_url: None,
            })
        }

        fn delete_partial(&mut self, channel_id: i64, message_id: i64) -> Result<(), DraftError> {
            self.partial_deletes.push((channel_id, message_id));
            Ok(())
        }

        fn rename_thread(&mut self, thread_id: i64, name: &str) -> Result<(), DraftError> {
            self.renamed_threads.push((thread_id, name.to_owned()));
            Ok(())
        }

        fn lock_thread(&mut self, thread_id: i64) -> Result<(), DraftError> {
            self.locked_threads.push(thread_id);
            Ok(())
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct InteractionFollowup {
        message: PublishedDraftMessage,
        payload: DraftMessagePayload,
        ephemeral: bool,
        deleted: bool,
    }

    #[derive(Clone, Debug)]
    struct RecordingInteraction {
        guild_id: Option<i64>,
        user_id: i64,
        user_name: String,
        channel_id: i64,
        deferred: bool,
        followups: Vec<InteractionFollowup>,
        responses: Vec<DraftResponse>,
        original: PublishedDraftMessage,
        edited_original: Vec<DraftMessagePayload>,
        fail_edit_original: bool,
    }

    impl RecordingInteraction {
        fn new(guild_id: i64, user_id: i64) -> Self {
            let channel_id = 555_000;
            Self {
                guild_id: Some(guild_id),
                user_id,
                user_name: "Draft Starter".to_owned(),
                channel_id,
                deferred: false,
                followups: Vec::new(),
                responses: Vec::new(),
                original: PublishedDraftMessage {
                    id: 9_000_001,
                    channel_id,
                    jump_url: Some(format!(
                        "https://discord.test/channels/0/{channel_id}/9000001"
                    )),
                },
                edited_original: Vec::new(),
                fail_edit_original: false,
            }
        }
    }

    impl DraftInteractionPort for RecordingInteraction {
        fn guild_id(&self) -> Option<i64> {
            self.guild_id
        }

        fn user_id(&self) -> i64 {
            self.user_id
        }

        fn user_display_name(&self) -> &str {
            &self.user_name
        }

        fn channel_id(&self) -> i64 {
            self.channel_id
        }

        fn defer(&mut self) -> bool {
            self.deferred = true;
            true
        }

        fn followup(
            &mut self,
            payload: DraftMessagePayload,
            ephemeral: bool,
        ) -> Result<PublishedDraftMessage, DraftError> {
            let id = 7_000_000 + i64::try_from(self.followups.len()).unwrap_or(0);
            let message = PublishedDraftMessage {
                id,
                channel_id: self.channel_id,
                jump_url: None,
            };
            self.followups.push(InteractionFollowup {
                message: message.clone(),
                payload,
                ephemeral,
                deleted: false,
            });
            Ok(message)
        }

        fn delete_followup(&mut self, message_id: i64) -> Result<(), DraftError> {
            if let Some(message) = self
                .followups
                .iter_mut()
                .find(|followup| followup.message.id == message_id)
            {
                message.deleted = true;
            }
            Ok(())
        }

        fn respond(&mut self, response: DraftResponse) -> Result<(), DraftError> {
            self.responses.push(response);
            Ok(())
        }

        fn edit_original(
            &mut self,
            payload: DraftMessagePayload,
        ) -> Result<PublishedDraftMessage, DraftError> {
            if self.fail_edit_original {
                return Err(DraftError::new("edit failed"));
            }
            self.edited_original.push(payload);
            Ok(self.original.clone())
        }

        fn original_message(&self) -> Option<PublishedDraftMessage> {
            Some(self.original.clone())
        }
    }

    type TestApplication =
        DraftApplication<MemoryDraftPlayers, RecordingLobby, RecordingMatch, RecordingPublication>;

    fn application_with_players(count: usize) -> (TestApplication, Vec<i64>) {
        let mut players = MemoryDraftPlayers::default();
        let player_ids = (0..count)
            .map(|index| 50_001 + i64::try_from(index).unwrap_or(0))
            .collect::<Vec<_>>();
        for (index, player_id) in player_ids.iter().copied().enumerate() {
            players
                .add(
                    TEST_GUILD_ID,
                    draft_player(player_id, 1400.0 + index as f64 * 30.0),
                )
                .unwrap();
        }
        let mut publication = RecordingPublication::default();
        publication.available_channels.insert(555_000);
        (
            DraftApplication::new(
                players,
                RecordingLobby::default(),
                RecordingMatch::default(),
                publication,
            ),
            player_ids,
        )
    }

    fn execute_request(player_ids: &[i64]) -> ExecuteDraftRequest {
        ExecuteDraftRequest {
            guild_id: Some(TEST_GUILD_ID),
            lobby_kind: LobbyKind::Open,
            regular_player_ids: player_ids.to_vec(),
            conditional_player_ids: Vec::new(),
            specified_captain1_id: None,
            specified_captain2_id: None,
        }
    }

    #[test]
    fn test_startdraft_command_infers_lowskill_membership() {
        assert_eq!(
            resolve_start_lobby_kind(&[LobbyKind::LowSkill], None),
            Ok(LobbyKind::LowSkill)
        );
    }

    #[test]
    fn test_startdraft_command_requires_a_lobby_choice_when_queued_in_both() {
        let response =
            resolve_start_lobby_kind(&[LobbyKind::Open, LobbyKind::LowSkill], None).unwrap_err();
        assert_eq!(response.content, AMBIGUOUS_LOBBY_MESSAGE);
        assert!(response.ephemeral);
    }

    #[test]
    fn test_startdraft_command_drafts_from_the_explicit_lobby_choice() {
        assert_eq!(
            resolve_start_lobby_kind(
                &[LobbyKind::Open, LobbyKind::LowSkill],
                Some(LobbyKind::LowSkill)
            ),
            Ok(LobbyKind::LowSkill)
        );
    }

    #[test]
    fn test_completed_pre_draft_choices_start_dynamic_rounds() {
        let (mut application, player_ids) = application_with_players(10);
        let mut state = DraftState::new(TEST_GUILD_ID);
        state.player_pool_ids.clone_from(&player_ids);
        for (index, player_id) in player_ids.iter().copied().enumerate() {
            state.player_pool_data.insert(
                player_id,
                cached("Player", 1400.0 + index as f64 * 30.0, &[]),
            );
        }
        state.captain1_id = Some(player_ids[0]);
        state.captain2_id = Some(player_ids[1]);
        state.captain1_rating = 1400.0;
        state.captain2_rating = 1430.0;
        state.radiant_captain_id = Some(player_ids[0]);
        state.dire_captain_id = Some(player_ids[1]);
        state.coinflip_winner_id = Some(player_ids[0]);
        state.winner_choice_type = Some("side".to_owned());
        state.loser_choice_value = Some("first".to_owned());
        application.complete_pre_draft_choices(&mut state);
        assert_eq!(state.phase, DraftPhase::Drafting);
        assert_eq!(state.radiant_player_ids, [player_ids[0]]);
        assert_eq!(state.dire_player_ids, [player_ids[1]]);
        assert_eq!(state.current_captain_id(), Some(player_ids[0]));
    }

    #[test]
    fn test_draft_embed_shows_round_order_and_team_totals() {
        let (mut application, _) = application_with_players(10);
        let mut state = dynamic_state(TEST_GUILD_ID, 1400.0, 1430.0);
        state.radiant_hero_pick_order = Some(1);
        state.dire_hero_pick_order = Some(2);
        state.start_player_draft();
        let embed = application.build_live_draft_embed(&state);
        assert!(embed.description.contains("Round 1/4"));
        assert!(embed.description.contains("Radiant → 🔴 Dire"));
        assert!(embed.description.contains("🟢 1400"));
        assert!(embed.description.contains("🔴 1430"));
        assert!(!embed.footer.unwrap_or_default().contains("Started by"));
    }

    #[test]
    fn test_succeeds_with_sixteen_players() {
        let (mut application, player_ids) = application_with_players(16);
        let custom_ratings = [1000.0, 1001.0]
            .into_iter()
            .chain((0..14).map(|index| 1500.0 + index as f64 * 100.0))
            .collect::<Vec<_>>();
        for (player_id, rating) in player_ids.iter().zip(custom_ratings) {
            application
                .players
                .players
                .get_mut(&(TEST_GUILD_ID, *player_id))
                .unwrap()
                .glicko_rating = Some(rating);
        }
        let before = application
            .players
            .exclusion_counts(&player_ids, TEST_GUILD_ID);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &execute_request(&player_ids))
                .unwrap()
        );
        let state = application
            .state_manager
            .get_state(Some(TEST_GUILD_ID))
            .unwrap();
        let state = lock_recover(&state);
        let expected = player_ids[..2]
            .iter()
            .chain(&player_ids[8..])
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            state
                .player_pool_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert_eq!(state.excluded_player_ids.len(), 6);
        assert_eq!(
            state.full_exclusion_increment_ids,
            state.excluded_player_ids
        );
        assert!(state.half_exclusion_increment_ids.is_empty());
        assert!([state.captain1_id, state.captain2_id].contains(&state.coinflip_winner_id));
        assert_eq!(
            application
                .players
                .exclusion_counts(&player_ids, TEST_GUILD_ID),
            before
        );
        assert_eq!(application.lobbies.reserve_calls.len(), 1);
        assert!(interaction.followups[0].deleted);
        let sent = application
            .publication
            .channel_messages(interaction.channel_id);
        assert_eq!(sent.len(), 2);
        assert!(
            sent[0]
                .payload
                .content
                .as_deref()
                .unwrap()
                .contains("🍽️ All You Can Feed draft starting!")
        );
        assert!(sent[1].payload.embed.is_some());
        assert_eq!(state.draft_message_id, Some(sent[1].message.id));
    }

    #[test]
    fn test_posts_captain_ping_then_opening_embed_to_lobby_channel() {
        let (mut application, player_ids) = application_with_players(10);
        application.lobbies.lobby_channel_id = Some(777_001);
        application.lobbies.origin_channel_id = Some(777_000);
        application
            .publication
            .available_channels
            .extend([777_000, 777_001]);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &execute_request(&player_ids))
                .unwrap()
        );
        let state = application
            .state_manager
            .get_state(Some(TEST_GUILD_ID))
            .unwrap();
        let state = lock_recover(&state);
        let lobby_messages = application.publication.channel_messages(777_001);
        assert_eq!(lobby_messages.len(), 2);
        assert_eq!(
            lobby_messages[0].payload.content.as_deref(),
            Some(
                format!(
                    "<@{}> <@{}> 🍽️ All You Can Feed draft starting!",
                    state.captain1_id.unwrap(),
                    state.captain2_id.unwrap()
                )
                .as_str()
            )
        );
        assert_eq!(
            lobby_messages[1]
                .payload
                .embed
                .as_ref()
                .unwrap()
                .footer
                .as_deref(),
            Some("Started by Draft Starter")
        );
        assert!(application.publication.channel_messages(777_000).is_empty());
        assert!(
            application
                .publication
                .channel_messages(interaction.channel_id)
                .is_empty()
        );
        assert_eq!(state.draft_channel_id, Some(777_001));
    }

    #[test]
    fn test_captain_ping_failure_does_not_block_draft_embed() {
        let (mut application, player_ids) = application_with_players(10);
        application.publication.fail_ping_channels.insert(555_000);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &execute_request(&player_ids))
                .unwrap()
        );
        let state = application
            .state_manager
            .get_state(Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(lock_recover(&state).captain_ping_message_id, None);
        let messages = application.publication.channel_messages(555_000);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].payload.embed.is_some());
    }

    #[test]
    fn test_unavailable_lobby_and_origin_channels_fall_back_to_interaction_channel() {
        let (mut application, player_ids) = application_with_players(10);
        application.lobbies.lobby_channel_id = Some(777_005);
        application.lobbies.origin_channel_id = Some(777_006);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &execute_request(&player_ids))
                .unwrap()
        );
        let state = application
            .state_manager
            .get_state(Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(
            lock_recover(&state).draft_channel_id,
            Some(interaction.channel_id)
        );
        assert_eq!(
            application
                .publication
                .channel_messages(interaction.channel_id)
                .len(),
            2
        );
    }

    #[test]
    fn test_unavailable_lobby_channel_falls_back_to_origin_channel() {
        let (mut application, player_ids) = application_with_players(10);
        application.lobbies.lobby_channel_id = Some(777_008);
        application.lobbies.origin_channel_id = Some(777_007);
        application.publication.available_channels.insert(777_007);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &execute_request(&player_ids))
                .unwrap()
        );
        let state = application
            .state_manager
            .get_state(Some(TEST_GUILD_ID))
            .unwrap();
        assert_eq!(lock_recover(&state).draft_channel_id, Some(777_007));
        assert_eq!(application.publication.channel_messages(777_007).len(), 2);
        assert!(
            application
                .publication
                .channel_messages(interaction.channel_id)
                .is_empty()
        );
    }

    #[test]
    fn test_legacy_conditional_players_are_ignored() {
        let (mut application, player_ids) = application_with_players(16);
        let regular_ids = player_ids[..14].to_vec();
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &execute_request(&regular_ids))
                .unwrap()
        );
        let state = application
            .state_manager
            .get_state(Some(TEST_GUILD_ID))
            .unwrap();
        let state = lock_recover(&state);
        assert!(
            state
                .player_pool_ids
                .iter()
                .all(|id| !player_ids[14..].contains(id))
        );
        assert_eq!(
            state.full_exclusion_increment_ids,
            state.excluded_player_ids
        );
        assert!(state.half_exclusion_increment_ids.is_empty());
    }

    #[test]
    fn test_roster_overrides_track_nonresponders_as_half_exclusions() {
        let (mut application, player_ids) = application_with_players(18);
        let mut request = execute_request(&player_ids[..16]);
        request.conditional_player_ids = player_ids[16..].to_vec();
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &request)
                .unwrap()
        );
        let state = application
            .state_manager
            .get_state(Some(TEST_GUILD_ID))
            .unwrap();
        let state = lock_recover(&state);
        assert_eq!(
            state
                .half_exclusion_increment_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            player_ids[16..].iter().copied().collect()
        );
        assert_eq!(
            state
                .full_exclusion_increment_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            player_ids[..16]
                .iter()
                .copied()
                .filter(|id| !state.player_pool_ids.contains(id))
                .collect()
        );
    }

    #[test]
    fn test_captain_eligibility_flags_do_not_limit_selection() {
        let (mut application, player_ids) = application_with_players(16);
        for (index, player_id) in player_ids.iter().copied().enumerate() {
            application
                .players
                .players
                .get_mut(&(TEST_GUILD_ID, player_id))
                .unwrap()
                .glicko_rating = Some(if index >= 14 {
                3000.0 + (index - 14) as f64
            } else {
                1000.0 + index as f64 * 100.0
            });
        }
        assert!(
            application
                .players
                .set_captain_eligible(player_ids[0], TEST_GUILD_ID, true)
        );
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &execute_request(&player_ids))
                .unwrap()
        );
        let state = application
            .state_manager
            .get_state(Some(TEST_GUILD_ID))
            .unwrap();
        let state = lock_recover(&state);
        assert!(![state.captain1_id, state.captain2_id].contains(&Some(player_ids[0])));
        assert_eq!(
            [state.captain1_id.unwrap(), state.captain2_id.unwrap()]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            player_ids[14..].iter().copied().collect()
        );
    }

    #[test]
    fn test_fails_when_draft_already_active() {
        let (mut application, player_ids) = application_with_players(10);
        let existing = application
            .state_manager
            .create_draft(Some(TEST_GUILD_ID), LobbyKind::Open)
            .unwrap();
        lock_recover(&existing).phase = DraftPhase::Drafting;
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            !application
                .execute_draft(&mut interaction, &execute_request(&player_ids))
                .unwrap()
        );
        assert!(Arc::ptr_eq(
            &application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .unwrap(),
            &existing
        ));
        assert!(interaction.followups[0].deleted);
        assert!(
            interaction
                .responses
                .last()
                .unwrap()
                .content
                .starts_with("❌")
        );
    }

    #[test]
    fn test_clears_state_when_draft_embed_fails() {
        let (mut application, player_ids) = application_with_players(10);
        application.publication.fail_embed_channels.insert(555_000);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        let error = application
            .execute_draft(&mut interaction, &execute_request(&player_ids))
            .unwrap_err();
        assert!(error.message.contains("send failure"));
        assert!(
            application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .is_none()
        );
        assert!(interaction.followups[0].deleted);
        assert_eq!(application.publication.partial_deletes.len(), 1);
        assert_eq!(application.lobbies.release_calls.len(), 1);
        assert_eq!(
            application.lobbies.release_calls[0]
                .player_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            player_ids.iter().copied().collect()
        );
    }

    #[test]
    fn test_starting_a_draft_does_not_evict_from_the_other_lobby() {
        let (mut application, player_ids) = application_with_players(16);
        let mut request = execute_request(&player_ids);
        request.lobby_kind = LobbyKind::LowSkill;
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &request)
                .unwrap()
        );
        assert!(application.lobbies.evict_calls.is_empty());
    }

    #[test]
    fn test_abandoning_a_draft_leaves_the_other_lobby_untouched() {
        let (mut application, player_ids) = application_with_players(10);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            application
                .execute_draft(&mut interaction, &execute_request(&player_ids))
                .unwrap()
        );
        let handle = application
            .state_manager
            .get_state(Some(TEST_GUILD_ID))
            .unwrap();
        application
            .release_draft_players(&lock_recover(&handle))
            .unwrap();
        assert_eq!(application.lobbies.release_calls.len(), 1);
        assert!(application.lobbies.evict_calls.is_empty());
    }

    #[test]
    fn test_failed_draft_creation_skips_eviction_and_releases_the_pool() {
        let (mut application, player_ids) = application_with_players(10);
        application
            .state_manager
            .create_draft(Some(TEST_GUILD_ID), LobbyKind::Open)
            .unwrap();
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, 444_001);
        assert!(
            !application
                .execute_draft(&mut interaction, &execute_request(&player_ids))
                .unwrap()
        );
        assert!(application.lobbies.evict_calls.is_empty());
        assert_eq!(application.lobbies.release_calls.len(), 1);
        assert_eq!(
            application.lobbies.release_calls[0]
                .player_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            player_ids.iter().copied().collect()
        );
        assert!(interaction.followups[0].deleted);
        assert!(
            interaction
                .responses
                .last()
                .unwrap()
                .content
                .starts_with("❌")
        );
    }

    fn final_pick_application() -> (TestApplication, DraftHandle, Vec<i64>) {
        let (application, player_ids) = application_with_players(10);
        let handle = application
            .state_manager
            .create_draft(Some(TEST_GUILD_ID), LobbyKind::Open)
            .unwrap();
        {
            let mut state = lock_recover(&handle);
            state.phase = DraftPhase::Drafting;
            state.player_pool_ids.clone_from(&player_ids);
            state.captain1_id = Some(player_ids[0]);
            state.captain2_id = Some(player_ids[1]);
            state.radiant_captain_id = Some(player_ids[0]);
            state.dire_captain_id = Some(player_ids[1]);
            state.captain1_rating = 1700.0;
            state.captain2_rating = 1600.0;
            state.radiant_hero_pick_order = Some(1);
            state.dire_hero_pick_order = Some(2);
            state.current_round_first_captain_id = Some(player_ids[1]);
            state.current_pick_index = 7;
            state.radiant_player_ids =
                vec![player_ids[0], player_ids[2], player_ids[5], player_ids[6]];
            state.dire_player_ids = vec![
                player_ids[1],
                player_ids[3],
                player_ids[4],
                player_ids[7],
                player_ids[8],
            ];
            state.draft_channel_id = Some(555_000);
            state.draft_message_id = Some(9_000_001);
        }
        (application, handle, player_ids)
    }

    fn mark_final_pick_complete(handle: &DraftHandle, player_id: i64) {
        let mut state = lock_recover(handle);
        let picker_id = state.captain1_id;
        assert!(state.pick_player(player_id, picker_id));
        assert_eq!(state.phase, DraftPhase::Complete);
    }

    fn set_interaction_channel(interaction: &mut RecordingInteraction, channel_id: i64) {
        interaction.channel_id = channel_id;
        interaction.original.channel_id = channel_id;
        interaction.original.jump_url = Some(format!(
            "https://discord.test/channels/0/{channel_id}/{}",
            interaction.original.id
        ));
    }

    #[test]
    fn test_stale_pick_callback_cannot_mutate_restarted_draft() {
        let (application, old_state, player_ids) = final_pick_application();
        let session_id = lock_recover(&old_state).session_id;
        let token = application
            .prepare_player_pick(
                TEST_GUILD_ID,
                player_ids[9],
                player_ids[0],
                Some(session_id),
            )
            .unwrap();
        assert!(
            application
                .state_manager
                .clear_state(Some(TEST_GUILD_ID), Some(&old_state))
                .is_some()
        );
        let restarted = application
            .state_manager
            .create_draft(Some(TEST_GUILD_ID), LobbyKind::Open)
            .unwrap();
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        assert_eq!(
            application
                .commit_player_pick_with_feedback(&mut interaction, token)
                .unwrap(),
            DraftPickCommit::Stale
        );
        assert!(Arc::ptr_eq(
            &application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .unwrap(),
            &restarted
        ));
        assert_eq!(lock_recover(&old_state).current_pick_index, 7);
        assert_eq!(
            interaction.followups[0].payload.content.as_deref(),
            Some("❌ This draft is no longer active — use the controls on the current draft.")
        );
        assert!(interaction.followups[0].ephemeral);
    }

    #[test]
    fn test_completion_evicts_the_matched_players_from_the_other_lobby() {
        let (mut application, handle, player_ids) = final_pick_application();
        mark_final_pick_complete(&handle, player_ids[9]);
        let drafted = {
            let state = lock_recover(&handle);
            state
                .radiant_player_ids
                .iter()
                .chain(&state.dire_player_ids)
                .copied()
                .collect::<BTreeSet<_>>()
        };
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        let outcome =
            application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        assert_eq!(outcome.pending_match_id, Some(1234));
        assert_eq!(application.lobbies.evict_calls.len(), 1);
        assert_eq!(
            application.lobbies.evict_calls[0]
                .player_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            drafted
        );
        assert_eq!(application.lobbies.evict_calls[0].kind, LobbyKind::Open);
    }

    #[test]
    fn test_eviction_failure_does_not_abort_a_completed_draft() {
        let (mut application, handle, player_ids) = final_pick_application();
        mark_final_pick_complete(&handle, player_ids[9]);
        application.lobbies.evict_error = Some(DraftError::new("simulated lobby eviction failure"));
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        let outcome =
            application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        assert_eq!(outcome.pending_match_id, Some(1234));
        assert_eq!(
            application
                .matches
                .pending
                .as_ref()
                .unwrap()
                .pending_match_id,
            Some(1234)
        );
        assert_eq!(
            application.lobbies.reset_calls,
            [(TEST_GUILD_ID, LobbyKind::Open)]
        );
        assert!(
            application
                .operation_log
                .iter()
                .any(|event| event.contains("simulated lobby eviction failure"))
        );
    }

    #[test]
    fn test_completion_embed_identifies_lobby_and_pending_match() {
        let (mut application, handle, player_ids) = final_pick_application();
        lock_recover(&handle).lobby_kind = LobbyKind::LowSkill;
        mark_final_pick_complete(&handle, player_ids[9]);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        let title = &interaction.edited_original[0].embed.as_ref().unwrap().title;
        assert!(title.contains(LobbyKind::LowSkill.display_name()));
        assert!(title.contains("Match #1234"));
    }

    #[test]
    fn test_pending_match_preserves_conditional_exclusions() {
        let (mut application, handle, player_ids) = final_pick_application();
        {
            let mut state = lock_recover(&handle);
            state.excluded_player_ids = vec![60_001, 60_002];
            state.full_exclusion_increment_ids = vec![60_001, 60_002];
            state.half_exclusion_increment_ids = vec![60_003, 60_004];
        }
        mark_final_pick_complete(&handle, player_ids[9]);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        let pending = application.matches.pending.as_ref().unwrap();
        assert_eq!(pending.excluded_player_ids, [60_001, 60_002]);
        assert_eq!(pending.excluded_conditional_player_ids, [60_003, 60_004]);
        assert_eq!(pending.half_exclusion_increment_ids, [60_003, 60_004]);
        assert!(pending.exclusion_updates_deferred);
    }

    #[test]
    fn test_final_pick_defers_then_edits_original_message() {
        let (mut application, handle, player_ids) = final_pick_application();
        lock_recover(&handle).lobby_kind = LobbyKind::LowSkill;
        let session_id = lock_recover(&handle).session_id;
        let token = application
            .prepare_player_pick(
                TEST_GUILD_ID,
                player_ids[9],
                player_ids[0],
                Some(session_id),
            )
            .unwrap();
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        assert!(interaction.defer());
        assert_eq!(
            application.commit_player_pick(token),
            DraftPickCommit::Picked { complete: true }
        );
        assert_eq!(lock_recover(&handle).phase, DraftPhase::Complete);
        let outcome =
            application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        assert!(interaction.deferred);
        assert_eq!(interaction.edited_original.len(), 1);
        assert!(interaction.edited_original[0].view.is_none());
        assert_eq!(outcome.pending_match_id, Some(1234));
        assert!(
            application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .is_none()
        );
        assert_eq!(
            application.lobbies.reset_calls,
            [(TEST_GUILD_ID, LobbyKind::LowSkill)]
        );
        assert_eq!(
            application.matches.reminder_calls,
            [(TEST_GUILD_ID, 1234, LobbyKind::LowSkill)]
        );
    }

    #[test]
    fn test_final_pick_refreshes_surviving_pool_lobby_after_reset() {
        let (mut application, handle, player_ids) = final_pick_application();
        mark_final_pick_complete(&handle, player_ids[9]);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        assert_eq!(application.operation_log, ["reset", "refresh"]);
        assert_eq!(
            application.lobbies.reset_calls,
            [(TEST_GUILD_ID, LobbyKind::Open)]
        );
        assert_eq!(application.lobbies.refresh_calls, [TEST_GUILD_ID]);
    }

    #[test]
    fn test_final_pick_still_refreshes_pool_once_on_completion_failure() {
        let (mut application, handle, player_ids) = final_pick_application();
        mark_final_pick_complete(&handle, player_ids[9]);
        application.matches.get_error = Some(DraftError::new("pending read failed"));
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        let outcome =
            application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        assert_eq!(outcome.pending_match_id, Some(1234));
        assert!(application.lobbies.reset_calls.is_empty());
        assert_eq!(application.lobbies.refresh_calls, [TEST_GUILD_ID]);
        assert_eq!(application.operation_log, ["refresh"]);
        assert_eq!(application.matches.reserve_calls, [(TEST_GUILD_ID, 1234)]);
    }

    #[test]
    fn test_final_pick_edits_lobby_draft_without_duplicate_and_copies_to_origin() {
        let (mut application, handle, player_ids) = final_pick_application();
        mark_final_pick_complete(&handle, player_ids[9]);
        application.lobbies.lobby_channel_id = Some(555_000);
        application.lobbies.origin_channel_id = Some(777_001);
        application.publication.available_channels.insert(777_001);
        application.publication.next_message_id = 100;
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        set_interaction_channel(&mut interaction, 555_000);
        let outcome =
            application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        assert_eq!(interaction.edited_original.len(), 1);
        assert!(application.publication.channel_messages(555_000).is_empty());
        let origin_messages = application.publication.channel_messages(777_001);
        assert_eq!(origin_messages.len(), 1);
        assert_eq!(
            origin_messages[0].payload.embed,
            interaction.edited_original[0].embed
        );
        assert_eq!(outcome.lobby_message.as_ref().unwrap().id, 9_000_001);
        assert_eq!(outcome.origin_message.as_ref().unwrap().channel_id, 777_001);
        let info = &application.matches.message_info[0];
        assert_eq!(info.message_id, Some(9_000_001));
        assert_eq!(info.channel_id, Some(555_000));
        assert_eq!(info.origin_channel_id, Some(777_001));
        assert_eq!(info.command_message_id, Some(9_000_101));
        assert_eq!(info.command_channel_id, Some(777_001));
        assert_eq!(
            application.lobbies.cooldown_clear_calls,
            [(TEST_GUILD_ID, LobbyKind::Open)]
        );
        let pending = application.matches.pending.as_ref().unwrap();
        assert_eq!(pending.origin_channel_id, Some(777_001));
    }

    #[test]
    fn test_lobby_channel_copy_failure_does_not_break_completion() {
        let (mut application, handle, player_ids) = final_pick_application();
        mark_final_pick_complete(&handle, player_ids[9]);
        application.lobbies.lobby_channel_id = Some(777_002);
        application.publication.available_channels.insert(777_002);
        application.publication.fail_embed_channels.insert(777_002);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        let outcome =
            application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        assert_eq!(outcome.pending_match_id, Some(1234));
        assert_eq!(interaction.edited_original.len(), 1);
        assert!(interaction.followups.is_empty());
        assert!(interaction.responses.is_empty());
        assert_eq!(application.publication.send_attempts.len(), 1);
        assert_eq!(application.publication.send_attempts[0].0, 777_002);
        assert!(application.publication.sent.is_empty());
        assert_eq!(
            application.matches.message_info[0].message_id,
            Some(9_000_001)
        );
        assert_eq!(application.matches.message_info[0].command_message_id, None);
        assert!(
            application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .is_none()
        );
    }

    #[test]
    fn test_final_pick_stores_thread_info_for_record_finalize() {
        let (mut application, handle, player_ids) = final_pick_application();
        mark_final_pick_complete(&handle, player_ids[9]);
        application.lobbies.thread_id = Some(777_001);
        application.publication.available_channels.insert(777_001);
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        let outcome =
            application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        assert_eq!(outcome.pending_match_id, Some(1234));
        assert_eq!(application.publication.renamed_threads.len(), 1);
        assert_eq!(application.publication.renamed_threads[0].0, 777_001);
        assert!(
            application.publication.renamed_threads[0]
                .1
                .contains("Draft Complete")
        );
        assert_eq!(application.publication.locked_threads, [777_001]);
        let pending = application.matches.pending.as_ref().unwrap();
        assert_eq!(pending.thread_shuffle_thread_id, Some(777_001));
        assert_eq!(pending.thread_shuffle_message_id, Some(9_000_001));
        assert_eq!(application.publication.channel_messages(777_001).len(), 1);
    }

    #[test]
    fn test_pick_rejected_when_turn_passes_during_defer() {
        let (application, player_ids) = application_with_players(4);
        let handle = application
            .state_manager
            .create_draft(Some(TEST_GUILD_ID), LobbyKind::Open)
            .unwrap();
        {
            let mut state = lock_recover(&handle);
            state.phase = DraftPhase::Drafting;
            state.player_pool_ids.clone_from(&player_ids);
            state.captain1_id = Some(player_ids[0]);
            state.captain2_id = Some(player_ids[1]);
            state.radiant_captain_id = Some(player_ids[0]);
            state.dire_captain_id = Some(player_ids[1]);
            state.radiant_player_ids = vec![player_ids[0]];
            state.dire_player_ids = vec![player_ids[1]];
            state.current_round_first_captain_id = Some(player_ids[0]);
        }
        let session_id = lock_recover(&handle).session_id;
        let token = application
            .prepare_player_pick(
                TEST_GUILD_ID,
                player_ids[2],
                player_ids[0],
                Some(session_id),
            )
            .unwrap();
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        assert!(interaction.defer());
        assert!(lock_recover(&handle).pick_player(player_ids[3], Some(player_ids[0])));
        assert_eq!(
            lock_recover(&handle).current_captain_id(),
            Some(player_ids[1])
        );
        assert_eq!(
            application
                .commit_player_pick_with_feedback(&mut interaction, token)
                .unwrap(),
            DraftPickCommit::Rejected
        );
        let state = lock_recover(&handle);
        assert!(!state.radiant_player_ids.contains(&player_ids[2]));
        assert!(!state.dire_player_ids.contains(&player_ids[2]));
        assert_eq!(interaction.followups.len(), 1);
        assert_eq!(
            interaction.followups[0].payload.content.as_deref(),
            Some("❌ Cannot pick that player. They may already be picked.")
        );
        assert!(interaction.followups[0].ephemeral);
    }

    #[test]
    fn test_double_click_winner_chose_side_only_advances_once() {
        let (mut application, _) = application_with_players(2);
        let handle = application
            .state_manager
            .create_draft(Some(TEST_GUILD_ID), LobbyKind::Open)
            .unwrap();
        let session_id = {
            let mut state = lock_recover(&handle);
            state.phase = DraftPhase::WinnerChoice;
            state.captain1_id = Some(100);
            state.captain2_id = Some(200);
            state.coinflip_winner_id = Some(100);
            state.session_id
        };
        assert_eq!(
            application.winner_chose_side(TEST_GUILD_ID, session_id),
            Ok(true)
        );
        assert_eq!(lock_recover(&handle).phase, DraftPhase::WinnerSideChoice);
        let rejection = application
            .winner_chose_side(TEST_GUILD_ID, session_id)
            .unwrap_err();
        assert_eq!(
            rejection,
            DraftResponse::ephemeral("❌ That choice has already been made.")
        );
        let state = lock_recover(&handle);
        assert_eq!(state.phase, DraftPhase::WinnerSideChoice);
        assert_eq!(state.winner_choice_type.as_deref(), Some("side"));
    }

    #[test]
    fn test_double_click_final_choice_only_starts_player_draft_once() {
        let (mut application, player_ids) = application_with_players(10);
        let handle = application
            .state_manager
            .create_draft(Some(TEST_GUILD_ID), LobbyKind::Open)
            .unwrap();
        let session_id = {
            let mut state = lock_recover(&handle);
            state.phase = DraftPhase::LoserChoice;
            state.player_pool_ids.clone_from(&player_ids);
            state.captain1_id = Some(player_ids[0]);
            state.captain2_id = Some(player_ids[1]);
            state.radiant_captain_id = Some(player_ids[0]);
            state.dire_captain_id = Some(player_ids[1]);
            state.coinflip_winner_id = Some(player_ids[0]);
            state.winner_choice_type = Some("side".to_owned());
            state.winner_choice_value = Some("radiant".to_owned());
            state.session_id
        };
        assert_eq!(
            application.final_hero_pick_choice(TEST_GUILD_ID, session_id, "first"),
            Ok(true)
        );
        let first_snapshot = lock_recover(&handle).clone();
        assert_eq!(first_snapshot.phase, DraftPhase::Drafting);
        assert_eq!(first_snapshot.radiant_player_ids, [player_ids[0]]);
        assert_eq!(first_snapshot.dire_player_ids, [player_ids[1]]);
        let rejection = application
            .final_hero_pick_choice(TEST_GUILD_ID, session_id, "first")
            .unwrap_err();
        assert_eq!(
            rejection,
            DraftResponse::ephemeral("❌ That choice has already been made.")
        );
        assert_eq!(*lock_recover(&handle), first_snapshot);
    }

    #[test]
    fn test_sixteen_players_continue_to_normal_shuffle() {
        let player_ids = (1..=16).collect::<Vec<_>>();
        let result = route_large_lobby_shuffle(&player_ids, &BTreeSet::new());
        assert!(!result.draft_called);
        assert!(result.response.is_none());
        let blocked = route_large_lobby_shuffle(&player_ids, &BTreeSet::from([16]));
        assert!(!blocked.draft_called);
        assert!(blocked.response.unwrap().ephemeral);
    }

    #[test]
    fn test_participant_and_captain_allowed() {
        let available = vec![draft_player(300, 1700.0)];
        let view = DraftViewModel::drafting(
            &available,
            Some(100),
            [100, 200],
            &[100, 200, 300],
            Some(77),
        );
        assert_eq!(
            view.interaction_check(100),
            DraftInteractionDecision::Allowed
        );
        assert_eq!(
            view.interaction_check(300),
            DraftInteractionDecision::Allowed
        );
        assert_eq!(view.timeout_seconds, DRAFTING_TIMEOUT_SECONDS);
        assert_eq!(view.state_session_id, Some(77));
        assert!(
            view.buttons
                .iter()
                .any(|button| button.custom_id == "draft_pick_300")
        );
        assert!(
            view.buttons
                .iter()
                .any(|button| button.custom_id == "draft_pref_radiant")
        );
    }

    #[test]
    fn test_non_participant_rejected() {
        let view = DraftViewModel::drafting(&[], Some(100), [100, 200], &[100, 200], Some(77));
        assert_eq!(
            view.interaction_check(999),
            DraftInteractionDecision::Rejected(DraftResponse::ephemeral(
                "❌ You are not part of this draft."
            ))
        );
    }

    #[test]
    fn test_empty_participant_ids_allows_all() {
        let view = DraftViewModel::drafting(&[], None, [], &[], None);
        assert!(view.participant_ids.is_empty());
        assert_eq!(
            view.interaction_check(i64::MAX),
            DraftInteractionDecision::Allowed
        );
    }

    #[test]
    fn test_restart_deletes_ping_from_persisted_draft_channel() {
        let (mut application, handle, player_ids) = final_pick_application();
        {
            let mut state = lock_recover(&handle);
            state.captain_ping_message_id = Some(8_000_001);
            state.draft_channel_id = Some(777_001);
        }
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        interaction.user_name = "Captain".to_owned();
        assert!(application.restart_draft(&mut interaction, true).unwrap());
        assert_eq!(application.operation_log, ["response", "ping_partial"]);
        assert_eq!(
            interaction.responses,
            [DraftResponse::public(concat!(
                "🔄 **Draft Restarted** by Captain\n\n",
                "The lobby has been preserved. Use `/draft start` to start a new draft."
            ))]
        );
        assert_eq!(
            application.publication.partial_deletes,
            [(777_001, 8_000_001)]
        );
        assert!(
            application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .is_none()
        );
        assert_eq!(application.lobbies.release_calls.len(), 1);
        assert_eq!(application.lobbies.release_calls[0].guild_id, TEST_GUILD_ID);
        assert_eq!(application.lobbies.release_calls[0].kind, LobbyKind::Open);
    }

    #[test]
    fn test_restart_does_not_clear_a_previous_drafts_pending_match() {
        let (mut application, _handle, player_ids) = final_pick_application();
        application.matches.pending = Some(PendingMatchState {
            pending_match_id: Some(4242),
            ..PendingMatchState::default()
        });
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        assert!(application.restart_draft(&mut interaction, true).unwrap());
        assert_eq!(
            application
                .matches
                .pending
                .as_ref()
                .unwrap()
                .pending_match_id,
            Some(4242)
        );
        assert!(application.matches.reserve_calls.is_empty());
        assert!(application.matches.message_info.is_empty());
    }

    #[test]
    fn test_restart_rejects_draft_while_completion_is_finalizing() {
        let (mut application, handle, player_ids) = final_pick_application();
        lock_recover(&handle).phase = DraftPhase::Complete;
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        assert!(!application.restart_draft(&mut interaction, true).unwrap());
        assert!(Arc::ptr_eq(
            &application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .unwrap(),
            &handle
        ));
        assert_eq!(
            interaction.responses,
            [DraftResponse::ephemeral(
                "⏳ Draft results are still being finalized. Please wait."
            )]
        );
        assert!(application.lobbies.release_calls.is_empty());
    }

    #[test]
    fn test_timeout_deletes_ping_from_persisted_draft_channel() {
        let (mut application, handle, _player_ids) = final_pick_application();
        {
            let mut state = lock_recover(&handle);
            state.captain_ping_message_id = Some(8_000_001);
            state.draft_message_id = Some(8_000_002);
            state.draft_channel_id = Some(777_001);
        }
        assert!(
            application
                .handle_timeout(TEST_GUILD_ID, None, None)
                .unwrap()
        );
        assert_eq!(application.operation_log, ["ping_partial", "draft_partial"]);
        assert_eq!(
            application.publication.partial_deletes,
            [(777_001, 8_000_001)]
        );
        assert_eq!(application.publication.partial_edits.len(), 1);
        assert_eq!(
            (
                application.publication.partial_edits[0].0,
                application.publication.partial_edits[0].1
            ),
            (777_001, 8_000_002)
        );
        let payload = &application.publication.partial_edits[0].2;
        assert_eq!(payload.embed, Some(timeout_embed()));
        assert!(payload.view.is_none());
        assert_eq!(payload.embed.as_ref().unwrap().title, "⏰ Draft Timed Out");
        assert_eq!(
            payload.embed.as_ref().unwrap().description,
            concat!(
                "The draft was automatically cancelled due to inactivity.\n\n",
                "The lobby has been preserved. Use `/draft start` to begin a new draft."
            )
        );
        assert!(
            application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .is_none()
        );
        assert_eq!(application.lobbies.release_calls.len(), 1);
    }

    #[test]
    fn test_live_draft_update_uses_partial_message() {
        let (mut application, _) = application_with_players(0);
        let embed = DraftEmbed {
            title: "Provided live draft embed".to_owned(),
            description: "same object-level payload".to_owned(),
            ..DraftEmbed::default()
        };
        let edited = application
            .update_draft_message(777_001, 8_000_002, embed.clone())
            .unwrap();
        assert_eq!(edited.channel_id, 777_001);
        assert_eq!(edited.id, 8_000_002);
        assert_eq!(application.operation_log, ["draft_partial"]);
        assert_eq!(application.publication.partial_edits.len(), 1);
        assert_eq!(
            application.publication.partial_edits[0].2.embed,
            Some(embed)
        );
    }

    #[test]
    fn test_stale_view_timeout_does_not_clear_restarted_draft() {
        let (mut application, old_state, _) = final_pick_application();
        let old_session_id = lock_recover(&old_state).session_id;
        let old_view_id = application.track_draft_view(TEST_GUILD_ID, old_session_id);
        assert!(
            application
                .state_manager
                .clear_state(Some(TEST_GUILD_ID), Some(&old_state))
                .is_some()
        );
        let restarted = application
            .state_manager
            .create_draft(Some(TEST_GUILD_ID), LobbyKind::Open)
            .unwrap();
        assert!(
            !application
                .handle_timeout(TEST_GUILD_ID, Some(old_view_id), Some(old_session_id))
                .unwrap()
        );
        assert!(Arc::ptr_eq(
            &application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .unwrap(),
            &restarted
        ));
        assert!(application.lobbies.release_calls.is_empty());
    }

    #[test]
    fn test_superseded_view_timeout_does_not_clear_state() {
        let (mut application, handle, _) = final_pick_application();
        let session_id = lock_recover(&handle).session_id;
        let old_view_id = application.track_draft_view(TEST_GUILD_ID, session_id);
        let current_view_id = application.track_draft_view(TEST_GUILD_ID, session_id);
        assert!(application.view_is_finished(old_view_id));
        assert!(!application.view_is_finished(current_view_id));
        assert!(
            !application
                .handle_timeout(TEST_GUILD_ID, Some(old_view_id), Some(session_id))
                .unwrap()
        );
        assert!(Arc::ptr_eq(
            &application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .unwrap(),
            &handle
        ));
    }

    #[test]
    fn test_current_view_timeout_still_clears_state() {
        let (mut application, handle, _) = final_pick_application();
        let session_id = lock_recover(&handle).session_id;
        let view_id = application.track_draft_view(TEST_GUILD_ID, session_id);
        assert!(
            application
                .handle_timeout(TEST_GUILD_ID, Some(view_id), Some(session_id))
                .unwrap()
        );
        assert!(application.view_is_finished(view_id));
        assert!(
            application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .is_none()
        );
        assert_eq!(application.lobbies.release_calls.len(), 1);
    }

    #[test]
    fn test_sample_view_timeout_does_not_clear_real_draft() {
        let (mut application, handle, _) = final_pick_application();
        let real_session_id = lock_recover(&handle).session_id;
        assert!(
            !application
                .handle_timeout(TEST_GUILD_ID, Some(99_999), Some(real_session_id + 1))
                .unwrap()
        );
        assert!(Arc::ptr_eq(
            &application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .unwrap(),
            &handle
        ));
        assert!(application.lobbies.release_calls.is_empty());
    }

    #[test]
    fn test_error_after_pending_match_creation_points_to_record_abort() {
        let (mut application, handle, player_ids) = final_pick_application();
        mark_final_pick_complete(&handle, player_ids[9]);
        application.matches.next_id = 4242;
        application.matches.get_error = Some(DraftError::new("announcement lookup failed"));
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        let outcome =
            application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        let expected = DraftResponse::ephemeral(concat!(
            "⚠️ Draft complete — Match #4242 was created, but an error occurred while announcing it. ",
            "Betting and recording still work: play the match and use `/record`, or use ",
            "`/record abort` to cancel it."
        ));
        let actual = completion_error_guidance(Some(4242));
        assert_eq!(actual, expected);
        let response = outcome.response.unwrap();
        assert_eq!(response, expected);
        assert_eq!(interaction.responses, [response]);
        assert_eq!(application.lobbies.refresh_calls, [TEST_GUILD_ID]);
        assert_eq!(application.lobbies.release_calls.len(), 1);
    }

    #[test]
    fn test_error_before_pending_match_creation_still_suggests_draft_start() {
        let (mut application, handle, player_ids) = final_pick_application();
        mark_final_pick_complete(&handle, player_ids[9]);
        application.matches.persist_error = Some(DraftError::new("pending creation failed"));
        let mut interaction = RecordingInteraction::new(TEST_GUILD_ID, player_ids[0]);
        let outcome =
            application.complete_draft(&mut interaction, &handle, CompletionContext::default());
        let expected = DraftResponse::ephemeral(
            "⚠️ Draft complete but encountered an error during match setup. Use `/draft start` to try again.",
        );
        assert_eq!(outcome.pending_match_id, None);
        assert_eq!(outcome.response, Some(expected.clone()));
        assert_eq!(interaction.responses, [expected]);
        assert!(application.lobbies.refresh_calls.is_empty());
        assert_eq!(application.lobbies.release_calls.len(), 1);
        assert!(
            application
                .state_manager
                .get_state(Some(TEST_GUILD_ID))
                .is_none()
        );
    }
}
