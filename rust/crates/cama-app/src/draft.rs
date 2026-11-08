//! Discord-independent Immortal Draft state, policy, and durable finalization.
//!
//! The runtime `/draft` provider drives this state machine and persists it
//! through [`DraftStatePersistencePort`].

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub use cama_db::draft_finalization::DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE;
pub use cama_db::draft_financial_execution::{
    DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE, DraftFinancialSetupRun,
};
pub use cama_db::draft_financial_setup::{
    DRAFT_FINANCIAL_SETUP_PLAN_VERSION, DraftFinancialEffectIntent, DraftFinancialEffectKind,
    DraftFinancialIntentStatus, DraftFinancialSetupPlan, DraftFinancialSetupPolicy,
    DraftInvestmentSnapshot, DraftWealthSnapshot,
};

/// Durable financial-setup stages that the live Draft provider may compose.
/// Publication metadata written after `SetupComplete` is deliberately outside
/// the executor's immutable pending-payload hash contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftFinancialSetupStage {
    Linked,
    SetupComplete,
}

pub const DRAFT_POOL_SIZE: usize = 10;
pub const DRAFT_TOTAL_PICKS: usize = 8;
pub const PRE_DRAFT_TIMEOUT_SECONDS: u64 = 300;
pub const DRAFTING_TIMEOUT_SECONDS: u64 = 600;
pub const DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION: u64 =
    cama_db::draft_finalization::DRAFT_FINALIZATION_PLAN_VERSION;

/// Return the lease owner used by this process for durable Draft finalization.
///
/// Guild/session identity alone is not a sufficient lease owner: a restarted
/// process may otherwise look like the worker that crashed.  The process
/// nonce is generated once and is included in every owner string.
#[must_use]
pub fn draft_finalization_process_owner(guild_id: i64, session_id: u64) -> String {
    static PROCESS_NONCE: OnceLock<String> = OnceLock::new();
    let nonce = PROCESS_NONCE.get_or_init(|| {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        format!("{}-{timestamp:x}", std::process::id())
    });
    format!("draft-live:{nonce}:{guild_id}:{session_id}")
}

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
    #[serde(default)]
    pub excluded_conditional_player_ids: Vec<i64>,
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
            excluded_conditional_player_ids: Vec::new(),
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
    pub fn conditional_exclusion_ids(&self) -> &[i64] {
        if self.excluded_conditional_player_ids.is_empty() {
            &self.half_exclusion_increment_ids
        } else {
            &self.excluded_conditional_player_ids
        }
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

/// One immutable Discord destination and its deterministic recovery identity.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DraftFinalizationDeliveryPlan {
    pub delivery_key: String,
    pub channel_id: Option<i64>,
    pub message_id: Option<i64>,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// Immutable inputs needed by a future finalization recovery worker.
///
/// The plan is committed in the same transaction as the pending match and
/// Draft-envelope link. Unknown fields round-trip through `extensions` so an
/// older process cannot silently erase a newer recovery contract.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DraftFinalizationPlan {
    pub schema_version: u64,
    pub completion_key: String,
    pub guild_id: i64,
    pub session_id: u64,
    pub pending_payload_sha256: String,
    pub started_at: i64,
    pub shuffle_timestamp: i64,
    pub bet_lock_until: i64,
    pub is_bomb_pot: bool,
    pub lobby_kind: LobbyKind,
    pub source: DraftFinalizationDeliveryPlan,
    pub lobby: DraftFinalizationDeliveryPlan,
    pub origin: DraftFinalizationDeliveryPlan,
    pub thread_embed: DraftFinalizationDeliveryPlan,
    pub thread_ping: DraftFinalizationDeliveryPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub financial_setup: Option<DraftFinancialSetupPlan>,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl DraftFinalizationPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        guild_id: i64,
        session_id: u64,
        pending_payload_json: &str,
        started_at: i64,
        shuffle_timestamp: i64,
        bet_lock_until: i64,
        is_bomb_pot: bool,
        lobby_kind: LobbyKind,
        source_channel_id: Option<i64>,
        source_message_id: Option<i64>,
        lobby_channel_id: Option<i64>,
        origin_channel_id: Option<i64>,
        thread_id: Option<i64>,
    ) -> Result<Self, DraftPersistenceError> {
        let completion_key = format!("draft:{guild_id}:{session_id}");
        // Match the live publisher's source suppression and channel
        // deduplication so recovery never creates a copy that normal
        // completion would have skipped.
        let lobby_channel_id = lobby_channel_id.filter(|id| Some(*id) != source_channel_id);
        let origin_channel_id = origin_channel_id
            .filter(|id| Some(*id) != source_channel_id && Some(*id) != lobby_channel_id);
        let delivery = |name: &str, channel_id, message_id| DraftFinalizationDeliveryPlan {
            delivery_key: format!("{completion_key}:{name}"),
            channel_id,
            message_id,
            extensions: BTreeMap::new(),
        };
        let plan = Self {
            // Still the legacy version: this constructor is the structural
            // template `DraftFinalizationPlanSeed::new` reuses, and the seed
            // stamps the current version on the way out. A legacy plan can no
            // longer be linked -- cama-db's link boundary refuses it, because
            // the financial executor would refuse it too.
            schema_version: cama_db::draft_finalization::DRAFT_FINALIZATION_LEGACY_PLAN_VERSION,
            completion_key: completion_key.clone(),
            guild_id,
            session_id,
            pending_payload_sha256: draft_pending_payload_sha256(pending_payload_json),
            started_at,
            shuffle_timestamp,
            bet_lock_until,
            is_bomb_pot,
            lobby_kind,
            source: delivery("source", source_channel_id, source_message_id),
            lobby: delivery("lobby", lobby_channel_id, None),
            origin: delivery("origin", origin_channel_id, None),
            thread_embed: delivery("thread-embed", thread_id, None),
            thread_ping: delivery("thread-ping", thread_id, None),
            financial_setup: None,
            extensions: BTreeMap::new(),
        };
        plan.validate(pending_payload_json)?;
        Ok(plan)
    }

    pub fn validate(&self, pending_payload_json: &str) -> Result<(), DraftPersistenceError> {
        self.validate_structure()?;
        if self.pending_payload_sha256 != draft_pending_payload_sha256(pending_payload_json) {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: "Draft finalization pending payload hash does not match".to_owned(),
            });
        }
        Ok(())
    }

    fn validate_structure(&self) -> Result<(), DraftPersistenceError> {
        if !matches!(
            self.schema_version,
            cama_db::draft_finalization::DRAFT_FINALIZATION_LEGACY_PLAN_VERSION
                | DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION
        ) {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: format!(
                    "unsupported Draft finalization plan version {}",
                    self.schema_version
                ),
            });
        }
        let expected_key = format!("draft:{}:{}", self.guild_id, self.session_id);
        if self.completion_key != expected_key {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: "Draft finalization completion key does not match its identity".to_owned(),
            });
        }
        if self.pending_payload_sha256.len() != 64
            || !self
                .pending_payload_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: "Draft finalization pending payload hash is not lowercase SHA-256"
                    .to_owned(),
            });
        }
        if self.started_at != self.shuffle_timestamp || self.bet_lock_until < self.shuffle_timestamp
        {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: "Draft finalization timestamps are inconsistent".to_owned(),
            });
        }
        match self.schema_version {
            cama_db::draft_finalization::DRAFT_FINALIZATION_LEGACY_PLAN_VERSION => {
                if self.financial_setup.is_some() {
                    return Err(DraftPersistenceError::InvalidEnvelope {
                        message: "legacy Draft finalization plan contains financial setup"
                            .to_owned(),
                    });
                }
            }
            DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION => {
                let financial = self.financial_setup.as_ref().ok_or_else(|| {
                    DraftPersistenceError::InvalidEnvelope {
                        message: "Draft finalization plan v2 is missing financial setup".to_owned(),
                    }
                })?;
                financial
                    .validate_for_identity(
                        self.guild_id,
                        self.session_id,
                        financial.pending_match_id,
                    )
                    .map_err(|error| DraftPersistenceError::InvalidEnvelope {
                        message: error.to_string(),
                    })?;
            }
            _ => unreachable!("validated Draft finalization plan version"),
        }
        for (name, delivery) in [
            ("source", &self.source),
            ("lobby", &self.lobby),
            ("origin", &self.origin),
            ("thread-embed", &self.thread_embed),
            ("thread-ping", &self.thread_ping),
        ] {
            if delivery.delivery_key != format!("{}:{name}", self.completion_key) {
                return Err(DraftPersistenceError::InvalidEnvelope {
                    message: format!("Draft finalization {name} delivery key is not deterministic"),
                });
            }
            if delivery.message_id.is_some() && delivery.channel_id.is_none() {
                return Err(DraftPersistenceError::InvalidEnvelope {
                    message: format!("Draft finalization {name} message has no channel"),
                });
            }
        }
        if self.lobby.channel_id.is_some() && self.lobby.channel_id == self.source.channel_id {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: "Draft finalization lobby destination duplicates the source".to_owned(),
            });
        }
        if self.origin.channel_id.is_some()
            && (self.origin.channel_id == self.source.channel_id
                || self.origin.channel_id == self.lobby.channel_id)
        {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: "Draft finalization origin destination is duplicated".to_owned(),
            });
        }
        Ok(())
    }

    pub fn to_json(&self, pending_payload_json: &str) -> Result<String, DraftPersistenceError> {
        self.validate(pending_payload_json)?;
        serde_json::to_string(self).map_err(|error| DraftPersistenceError::Serialization {
            message: error.to_string(),
        })
    }

    pub fn from_json(raw: &str) -> Result<Self, DraftPersistenceError> {
        let plan: Self =
            serde_json::from_str(raw).map_err(|error| DraftPersistenceError::Serialization {
                message: error.to_string(),
            })?;
        plan.validate_structure()?;
        Ok(plan)
    }
}

/// Schema-v2 request expanded by SQLite into a complete immutable financial
/// plan inside the pending-link writer transaction.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DraftFinalizationPlanSeed {
    pub schema_version: u64,
    pub completion_key: String,
    pub guild_id: i64,
    pub session_id: u64,
    pub pending_payload_sha256: String,
    pub started_at: i64,
    pub shuffle_timestamp: i64,
    pub bet_lock_until: i64,
    pub is_bomb_pot: bool,
    pub lobby_kind: LobbyKind,
    pub source: DraftFinalizationDeliveryPlan,
    pub lobby: DraftFinalizationDeliveryPlan,
    pub origin: DraftFinalizationDeliveryPlan,
    pub thread_embed: DraftFinalizationDeliveryPlan,
    pub thread_ping: DraftFinalizationDeliveryPlan,
    pub financial_setup: DraftFinancialSetupPolicy,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl DraftFinalizationPlanSeed {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        guild_id: i64,
        session_id: u64,
        pending_payload_json: &str,
        started_at: i64,
        shuffle_timestamp: i64,
        bet_lock_until: i64,
        is_bomb_pot: bool,
        lobby_kind: LobbyKind,
        source_channel_id: Option<i64>,
        source_message_id: Option<i64>,
        lobby_channel_id: Option<i64>,
        origin_channel_id: Option<i64>,
        thread_id: Option<i64>,
        financial_setup: DraftFinancialSetupPolicy,
    ) -> Result<Self, DraftPersistenceError> {
        let legacy = DraftFinalizationPlan::new(
            guild_id,
            session_id,
            pending_payload_json,
            started_at,
            shuffle_timestamp,
            bet_lock_until,
            is_bomb_pot,
            lobby_kind,
            source_channel_id,
            source_message_id,
            lobby_channel_id,
            origin_channel_id,
            thread_id,
        )?;
        let seed = Self {
            schema_version: DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION,
            completion_key: legacy.completion_key,
            guild_id,
            session_id,
            pending_payload_sha256: legacy.pending_payload_sha256,
            started_at,
            shuffle_timestamp,
            bet_lock_until,
            is_bomb_pot,
            lobby_kind,
            source: legacy.source,
            lobby: legacy.lobby,
            origin: legacy.origin,
            thread_embed: legacy.thread_embed,
            thread_ping: legacy.thread_ping,
            financial_setup,
            extensions: BTreeMap::new(),
        };
        seed.validate(pending_payload_json)?;
        Ok(seed)
    }

    pub fn validate(&self, pending_payload_json: &str) -> Result<(), DraftPersistenceError> {
        if self.schema_version != DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: format!(
                    "unsupported Draft finalization plan-seed version {}",
                    self.schema_version
                ),
            });
        }
        let structural = DraftFinalizationPlan {
            schema_version: cama_db::draft_finalization::DRAFT_FINALIZATION_LEGACY_PLAN_VERSION,
            completion_key: self.completion_key.clone(),
            guild_id: self.guild_id,
            session_id: self.session_id,
            pending_payload_sha256: self.pending_payload_sha256.clone(),
            started_at: self.started_at,
            shuffle_timestamp: self.shuffle_timestamp,
            bet_lock_until: self.bet_lock_until,
            is_bomb_pot: self.is_bomb_pot,
            lobby_kind: self.lobby_kind,
            source: self.source.clone(),
            lobby: self.lobby.clone(),
            origin: self.origin.clone(),
            thread_embed: self.thread_embed.clone(),
            thread_ping: self.thread_ping.clone(),
            financial_setup: None,
            extensions: self.extensions.clone(),
        };
        structural.validate(pending_payload_json)?;
        self.financial_setup
            .validate()
            .map_err(|error| DraftPersistenceError::InvalidEnvelope {
                message: error.to_string(),
            })
    }

    pub fn to_json(&self, pending_payload_json: &str) -> Result<String, DraftPersistenceError> {
        self.validate(pending_payload_json)?;
        serde_json::to_string(self).map_err(|error| DraftPersistenceError::Serialization {
            message: error.to_string(),
        })
    }

    pub fn from_json(raw: &str) -> Result<Self, DraftPersistenceError> {
        let seed: Self =
            serde_json::from_str(raw).map_err(|error| DraftPersistenceError::Serialization {
                message: error.to_string(),
            })?;
        if seed.schema_version != DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION {
            return Err(DraftPersistenceError::InvalidEnvelope {
                message: format!(
                    "unsupported Draft finalization plan-seed version {}",
                    seed.schema_version
                ),
            });
        }
        seed.financial_setup.validate().map_err(|error| {
            DraftPersistenceError::InvalidEnvelope {
                message: error.to_string(),
            }
        })?;
        Ok(seed)
    }
}

#[must_use]
pub fn draft_pending_payload_sha256(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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

impl From<cama_db::draft_finalization::DraftFinalizationError> for DraftPersistenceError {
    fn from(error: cama_db::draft_finalization::DraftFinalizationError) -> Self {
        use cama_db::draft_finalization::DraftFinalizationError as DatabaseError;
        match error {
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
            DatabaseError::InvalidDraftEnvelope(message) => Self::InvalidEnvelope { message },
            DatabaseError::InvalidPendingPayload(message) => Self::Serialization { message },
            DatabaseError::InvalidPlan(message) | DatabaseError::InvalidProgress(message) => {
                Self::Serialization { message }
            }
            DatabaseError::Conflict { guild_id, reason } => Self::Conflict { guild_id, reason },
            DatabaseError::JobNotFound { completion_key } => Self::Backend {
                message: format!("draft finalization job {completion_key} does not exist"),
            },
            DatabaseError::StaleJobRevision {
                completion_key,
                expected,
                actual,
            } => Self::Backend {
                message: format!(
                    "draft finalization job revision is stale for {completion_key}: expected {expected}, found {actual}"
                ),
            },
            DatabaseError::StaleJobStage {
                completion_key,
                expected,
                actual,
            } => Self::Backend {
                message: format!(
                    "draft finalization job stage is stale for {completion_key}: expected {expected}, found {actual}"
                ),
            },
            DatabaseError::LeaseHeld {
                completion_key,
                owner,
                lease_until,
            } => Self::Backend {
                message: format!(
                    "draft finalization job {completion_key} is leased by {owner} until {lease_until}"
                ),
            },
            DatabaseError::LeaseLost {
                completion_key,
                expected_owner,
                actual_owner,
                lease_until,
            } => Self::Backend {
                message: format!(
                    "draft finalization job {completion_key} lease is not held by {expected_owner}: current owner {actual_owner:?}, lease until {lease_until:?}"
                ),
            },
            DatabaseError::Sqlite(error) => Self::Backend {
                message: error.to_string(),
            },
        }
    }
}

/// Result of atomically finding or creating a Draft's pending match and
/// linking it into the durable finalization envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftPendingMatchLink {
    pub envelope: DraftStateEnvelope,
    pub completion_key: String,
    pub pending_match_id: i64,
    pub pending_payload_json: String,
    pub plan_json: String,
    pub pending_created: bool,
    pub job_created: bool,
}

/// Durable finalization-job identity and progress used by the runtime
/// publication reconciler.  The SQLite repository owns leasing and CAS;
/// application code owns the forward-compatible progress JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftFinalizationJobSnapshot {
    pub completion_key: String,
    pub guild_id: i64,
    pub session_id: u64,
    pub pending_match_id: i64,
    pub revision: u64,
    pub stage: String,
    pub progress_json: String,
    pub lease_owner: Option<String>,
    pub lease_until: Option<i64>,
}

/// Persistence boundary shared by the Draft runtime and recovery observers.
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

    /// Atomically create or recover the pending match for a fenced Complete
    /// Draft and write its identity back to the durable envelope.
    fn link_finalizing_pending_match(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        pending_payload_json: &str,
        plan_json: &str,
    ) -> Result<DraftPendingMatchLink, DraftPersistenceError> {
        let _ = (
            expected_session_id,
            expected_revision,
            pending_payload_json,
            plan_json,
        );
        Err(DraftPersistenceError::Backend {
            message: format!(
                "draft finalization linking is not supported for guild {guild_id} by this persistence port"
            ),
        })
    }

    /// Atomically link a pending match and expand a schema-v2 financial seed
    /// from the same SQLite writer snapshot.
    fn link_finalizing_pending_match_v2(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        pending_payload_json: &str,
        plan_seed_json: &str,
    ) -> Result<DraftPendingMatchLink, DraftPersistenceError> {
        let _ = (
            expected_session_id,
            expected_revision,
            pending_payload_json,
            plan_seed_json,
        );
        Err(DraftPersistenceError::Backend {
            message: format!(
                "Draft financial plan linking is not supported for guild {guild_id} by this persistence port"
            ),
        })
    }

    /// Load the exact pending-match JSON already named by a finalizing Draft.
    /// Implementations must preserve the stored bytes so idempotent retries do
    /// not discard forward-compatible pending-match fields.
    fn linked_pending_payload(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<String, DraftPersistenceError> {
        let _ = (session_id, pending_match_id);
        Err(DraftPersistenceError::Backend {
            message: format!(
                "raw pending-match recovery is not supported for guild {guild_id} by this persistence port"
            ),
        })
    }

    /// Load the exact immutable plan attached to a linked finalization job.
    fn linked_finalization_plan(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<String, DraftPersistenceError> {
        let _ = (session_id, pending_match_id);
        Err(DraftPersistenceError::Backend {
            message: format!(
                "raw Draft finalization plan recovery is not supported for guild {guild_id} by this persistence port"
            ),
        })
    }

    /// Load the typed financial stage without replaying the immutable setup
    /// executor. This lets later publication metadata coexist with a finished
    /// setup while keeping `linked` execution hash-checked.
    fn finalizing_financial_setup_stage(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<DraftFinancialSetupStage, DraftPersistenceError> {
        let _ = (session_id, pending_match_id);
        Err(DraftPersistenceError::Backend {
            message: format!(
                "Draft financial stage loading is not supported for guild {guild_id} by this persistence port"
            ),
        })
    }

    /// Claim or renew this session's finalization lease and atomically apply
    /// (or validate the idempotent replay of) its frozen financial setup.
    fn run_finalizing_financial_setup(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
        now: i64,
    ) -> Result<DraftFinancialSetupRun, DraftPersistenceError> {
        let _ = (session_id, pending_match_id, now);
        Err(DraftPersistenceError::Backend {
            message: format!(
                "Draft financial setup is not supported for guild {guild_id} by this persistence port"
            ),
        })
    }

    /// Load the durable publication job without mutating its lease.  Older
    /// persistence adapters may omit publication recovery and retain the
    /// source-compatible default error.
    fn finalization_job(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<Option<DraftFinalizationJobSnapshot>, DraftPersistenceError> {
        let _ = (session_id, pending_match_id);
        Err(DraftPersistenceError::Backend {
            message: format!(
                "Draft finalization job recovery is not supported for guild {guild_id}"
            ),
        })
    }

    /// Claim or renew the publication worker lease under a revision CAS.
    #[allow(clippy::too_many_arguments)]
    fn claim_finalization_job(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
        expected_revision: u64,
        owner: &str,
        now: i64,
        lease_until: i64,
    ) -> Result<DraftFinalizationJobSnapshot, DraftPersistenceError> {
        let _ = (
            session_id,
            pending_match_id,
            expected_revision,
            owner,
            now,
            lease_until,
        );
        Err(DraftPersistenceError::Backend {
            message: format!(
                "Draft finalization job leasing is not supported for guild {guild_id}"
            ),
        })
    }

    /// Persist one idempotent publication receipt (or any forward-compatible
    /// recovery marker) under a leased stage/revision CAS.  The stage may be
    /// unchanged when only progress advances.
    #[allow(clippy::too_many_arguments)]
    fn advance_finalization_job(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
        expected_revision: u64,
        expected_stage: &str,
        next_stage: &str,
        progress_patch_json: &str,
        owner: &str,
        now: i64,
    ) -> Result<DraftFinalizationJobSnapshot, DraftPersistenceError> {
        let _ = (
            session_id,
            pending_match_id,
            expected_revision,
            expected_stage,
            next_stage,
            progress_patch_json,
            owner,
            now,
        );
        Err(DraftPersistenceError::Backend {
            message: format!("Draft finalization progress is not supported for guild {guild_id}"),
        })
    }

    /// Mark the linked finalization job terminal after the live provider has
    /// completed every required downstream operation. Implementations must
    /// lease and CAS the job so a stale worker cannot hide recoverable work.
    fn complete_finalization_job(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
        expected_draft_revision: u64,
        now: i64,
    ) -> Result<(), DraftPersistenceError> {
        let _ = (session_id, pending_match_id, expected_draft_revision, now);
        Err(DraftPersistenceError::Backend {
            message: format!(
                "Draft finalization completion is not supported for guild {guild_id} by this persistence port"
            ),
        })
    }
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
    finalization: cama_db::draft_finalization::DraftFinalizationRepository,
}

/// Short compatibility alias for the SQLite draft persistence adapter.
pub type SqliteDraftPersistence = SqliteDraftStatePersistence;

impl SqliteDraftStatePersistence {
    /// Build an adapter for an already migrated database.
    #[must_use]
    pub fn new(path: impl AsRef<std::path::Path>) -> Self {
        let path = path.as_ref();
        Self {
            repository: cama_db::draft_state::DraftStateRepository::new(path),
            finalization: cama_db::draft_finalization::DraftFinalizationRepository::new(path),
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

    fn link_finalizing_pending_match(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        pending_payload_json: &str,
        plan_json: &str,
    ) -> Result<DraftPendingMatchLink, DraftPersistenceError> {
        DraftFinalizationPlan::from_json(plan_json)?;
        let linked = self.finalization.link_pending_match_validated(
            guild_id,
            expected_session_id,
            expected_revision,
            pending_payload_json,
            plan_json,
            |raw| {
                DraftStateEnvelope::from_json(raw)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
        )?;
        Ok(DraftPendingMatchLink {
            envelope: Self::decode(linked.draft)?,
            completion_key: linked.completion_key,
            pending_match_id: linked.pending_match_id,
            pending_payload_json: linked.pending_payload_json,
            plan_json: linked.job.plan_json,
            pending_created: linked.pending_created,
            job_created: linked.job_created,
        })
    }

    fn link_finalizing_pending_match_v2(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        pending_payload_json: &str,
        plan_seed_json: &str,
    ) -> Result<DraftPendingMatchLink, DraftPersistenceError> {
        let seed = DraftFinalizationPlanSeed::from_json(plan_seed_json)?;
        seed.validate(pending_payload_json)?;
        let linked = self
            .finalization
            .link_pending_match_with_financial_plan_validated(
                guild_id,
                expected_session_id,
                expected_revision,
                pending_payload_json,
                plan_seed_json,
                |raw| {
                    DraftStateEnvelope::from_json(raw)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                },
            )?;
        let plan = DraftFinalizationPlan::from_json(&linked.job.plan_json)?;
        plan.validate(&linked.pending_payload_json)?;
        Ok(DraftPendingMatchLink {
            envelope: Self::decode(linked.draft)?,
            completion_key: linked.completion_key,
            pending_match_id: linked.pending_match_id,
            pending_payload_json: linked.pending_payload_json,
            plan_json: linked.job.plan_json,
            pending_created: linked.pending_created,
            job_created: linked.job_created,
        })
    }

    fn linked_pending_payload(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<String, DraftPersistenceError> {
        self.finalization
            .linked_pending_payload(guild_id, session_id, pending_match_id)
            .map_err(Into::into)
    }

    fn linked_finalization_plan(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<String, DraftPersistenceError> {
        // A finalizing envelope created before the job-table migration has no
        // immutable destination snapshot. JobNotFound is intentionally
        // surfaced for manual recovery instead of fabricating a lossy plan.
        self.finalization
            .linked_plan_json(guild_id, session_id, pending_match_id)
            .map_err(Into::into)
    }

    fn finalizing_financial_setup_stage(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<DraftFinancialSetupStage, DraftPersistenceError> {
        use cama_db::draft_finalization::{
            DRAFT_FINALIZATION_INITIAL_STAGE, DraftFinalizationError, draft_completion_key,
        };

        let completion_key = draft_completion_key(guild_id, session_id);
        let Some(job) = self.finalization.job(&completion_key)? else {
            return Err(DraftFinalizationError::JobNotFound { completion_key }.into());
        };
        if job.guild_id != guild_id
            || job.session_id != session_id
            || job.pending_match_id != pending_match_id
        {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "Draft financial stage job identity does not match".to_owned(),
            });
        }
        match job.stage.as_str() {
            DRAFT_FINALIZATION_INITIAL_STAGE => Ok(DraftFinancialSetupStage::Linked),
            DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE => Ok(DraftFinancialSetupStage::SetupComplete),
            stage => Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: format!("Draft financial setup job is at unexpected stage {stage}"),
            }),
        }
    }

    fn run_finalizing_financial_setup(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
        now: i64,
    ) -> Result<DraftFinancialSetupRun, DraftPersistenceError> {
        use cama_db::draft_finalization::{DraftFinalizationError, draft_completion_key};

        let completion_key = draft_completion_key(guild_id, session_id);
        let Some(job) = self.finalization.job(&completion_key)? else {
            return Err(DraftFinalizationError::JobNotFound { completion_key }.into());
        };
        if job.guild_id != guild_id
            || job.session_id != session_id
            || job.pending_match_id != pending_match_id
        {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "Draft financial setup job identity does not match".to_owned(),
            });
        }
        if !matches!(
            job.stage.as_str(),
            cama_db::draft_finalization::DRAFT_FINALIZATION_INITIAL_STAGE
                | DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE
        ) {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: format!(
                    "Draft financial setup job is at unexpected stage {}",
                    job.stage
                ),
            });
        }
        let lease_owner = draft_finalization_process_owner(guild_id, session_id);
        let lease_until = now
            .checked_add(60)
            .ok_or_else(|| DraftPersistenceError::Conflict {
                guild_id,
                reason: "Draft financial setup lease deadline overflowed".to_owned(),
            })?;
        let claimed = self.finalization.claim_job_lease_for_identity(
            &job.completion_key,
            job.revision,
            &lease_owner,
            now,
            lease_until,
            guild_id,
            session_id,
            pending_match_id,
        )?;
        self.finalization
            .run_leased_financial_setup(&job.completion_key, claimed.revision, &lease_owner, now)
            .map_err(|error| match error {
                cama_db::draft_financial_execution::DraftFinancialExecutionError::JobNotFound {
                    completion_key,
                } => DraftPersistenceError::Backend {
                    message: format!("draft finalization job {completion_key} does not exist"),
                },
                cama_db::draft_financial_execution::DraftFinancialExecutionError::InvalidPlan(
                    message,
                ) => DraftPersistenceError::InvalidEnvelope { message },
                cama_db::draft_financial_execution::DraftFinancialExecutionError::Conflict {
                    reason,
                    ..
                } => DraftPersistenceError::Conflict { guild_id, reason },
                other => DraftPersistenceError::Backend {
                    message: other.to_string(),
                },
            })
    }

    fn finalization_job(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<Option<DraftFinalizationJobSnapshot>, DraftPersistenceError> {
        use cama_db::draft_finalization::draft_completion_key;

        let completion_key = draft_completion_key(guild_id, session_id);
        let Some(job) = self.finalization.job(&completion_key)? else {
            return Ok(None);
        };
        if job.guild_id != guild_id
            || job.session_id != session_id
            || job.pending_match_id != pending_match_id
        {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "Draft finalization job identity does not match recovery row".to_owned(),
            });
        }
        Ok(Some(DraftFinalizationJobSnapshot {
            completion_key: job.completion_key,
            guild_id: job.guild_id,
            session_id: job.session_id,
            pending_match_id: job.pending_match_id,
            revision: job.revision,
            stage: job.stage,
            progress_json: job.progress_json,
            lease_owner: job.lease_owner,
            lease_until: job.lease_until,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn claim_finalization_job(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
        expected_revision: u64,
        owner: &str,
        now: i64,
        lease_until: i64,
    ) -> Result<DraftFinalizationJobSnapshot, DraftPersistenceError> {
        use cama_db::draft_finalization::{DraftFinalizationError, draft_completion_key};

        let completion_key = draft_completion_key(guild_id, session_id);
        let Some(job) = self.finalization.job(&completion_key)? else {
            return Err(DraftFinalizationError::JobNotFound { completion_key }.into());
        };
        if job.guild_id != guild_id
            || job.session_id != session_id
            || job.pending_match_id != pending_match_id
        {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "Draft finalization lease identity does not match recovery row".to_owned(),
            });
        }
        let claimed = self.finalization.claim_job_lease_for_identity(
            &completion_key,
            expected_revision,
            owner,
            now,
            lease_until,
            guild_id,
            session_id,
            pending_match_id,
        )?;
        if claimed.guild_id != guild_id
            || claimed.session_id != session_id
            || claimed.pending_match_id != pending_match_id
        {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "claimed Draft finalization job identity does not match recovery row"
                    .to_owned(),
            });
        }
        Ok(DraftFinalizationJobSnapshot {
            completion_key: claimed.completion_key,
            guild_id: claimed.guild_id,
            session_id: claimed.session_id,
            pending_match_id: claimed.pending_match_id,
            revision: claimed.revision,
            stage: claimed.stage,
            progress_json: claimed.progress_json,
            lease_owner: claimed.lease_owner,
            lease_until: claimed.lease_until,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn advance_finalization_job(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
        expected_revision: u64,
        expected_stage: &str,
        next_stage: &str,
        progress_patch_json: &str,
        owner: &str,
        now: i64,
    ) -> Result<DraftFinalizationJobSnapshot, DraftPersistenceError> {
        use cama_db::draft_finalization::{DraftFinalizationError, draft_completion_key};

        let completion_key = draft_completion_key(guild_id, session_id);
        let Some(job) = self.finalization.job(&completion_key)? else {
            return Err(DraftFinalizationError::JobNotFound { completion_key }.into());
        };
        if job.guild_id != guild_id
            || job.session_id != session_id
            || job.pending_match_id != pending_match_id
        {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "advanced Draft finalization job identity does not match recovery row"
                    .to_owned(),
            });
        }
        let advanced = self.finalization.advance_job_stage_for_identity(
            &completion_key,
            expected_revision,
            expected_stage,
            next_stage,
            progress_patch_json,
            owner,
            now,
            guild_id,
            session_id,
            pending_match_id,
        )?;
        if advanced.guild_id != guild_id
            || advanced.session_id != session_id
            || advanced.pending_match_id != pending_match_id
        {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "advanced Draft finalization job identity does not match recovery row"
                    .to_owned(),
            });
        }
        Ok(DraftFinalizationJobSnapshot {
            completion_key: advanced.completion_key,
            guild_id: advanced.guild_id,
            session_id: advanced.session_id,
            pending_match_id: advanced.pending_match_id,
            revision: advanced.revision,
            stage: advanced.stage,
            progress_json: advanced.progress_json,
            lease_owner: advanced.lease_owner,
            lease_until: advanced.lease_until,
        })
    }

    fn complete_finalization_job(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
        expected_draft_revision: u64,
        now: i64,
    ) -> Result<(), DraftPersistenceError> {
        use cama_db::draft_finalization::{
            DRAFT_FINALIZATION_COMPLETE_STAGE, DraftFinalizationError, draft_completion_key,
        };

        let completion_key = draft_completion_key(guild_id, session_id);
        let Some(job) = self.finalization.job(&completion_key)? else {
            return Err(DraftFinalizationError::JobNotFound { completion_key }.into());
        };
        if job.guild_id != guild_id
            || job.session_id != session_id
            || job.pending_match_id != pending_match_id
        {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "Draft finalization job identity does not match completion".to_owned(),
            });
        }
        if job.stage == DRAFT_FINALIZATION_COMPLETE_STAGE {
            return Ok(());
        }
        if job.stage != cama_db::draft_finalization::DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: format!(
                    "Draft finalization job must be published before terminal completion; found {}",
                    job.stage
                ),
            });
        }
        let lease_owner = draft_finalization_process_owner(guild_id, session_id);
        let lease_until = now
            .checked_add(60)
            .ok_or_else(|| DraftPersistenceError::Conflict {
                guild_id,
                reason: "Draft finalization lease deadline overflowed".to_owned(),
            })?;
        let claimed = self.finalization.claim_job_lease_for_identity(
            &job.completion_key,
            job.revision,
            &lease_owner,
            now,
            lease_until,
            guild_id,
            session_id,
            pending_match_id,
        )?;
        let completed = self.finalization.complete_job_and_delete_draft(
            &job.completion_key,
            claimed.revision,
            &lease_owner,
            now,
            guild_id,
            session_id,
            pending_match_id,
            expected_draft_revision,
            job.stage.as_str(),
        )?;
        if completed.guild_id != guild_id
            || completed.session_id != session_id
            || completed.pending_match_id != pending_match_id
            || completed.stage != DRAFT_FINALIZATION_COMPLETE_STAGE
        {
            return Err(DraftPersistenceError::Conflict {
                guild_id,
                reason: "Draft finalization completion returned the wrong job".to_owned(),
            });
        }
        Ok(())
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

/// Production coin: an unbiased random flip, matching the legacy
/// `random.choice([captain1_id, captain2_id])` contract.
///
/// The flip decides first pick, so it must not be predictable from earlier
/// drafts in this or any other guild.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RandomDraftCoin;

impl DraftCoinPort for RandomDraftCoin {
    fn choose(&mut self, captain1_id: i64, captain2_id: i64) -> i64 {
        if fastrand::bool() {
            captain2_id
        } else {
            captain1_id
        }
    }
}

/// Deterministic strict alternation, for tests that need a fixed flip.
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
            coin: Box::new(RandomDraftCoin),
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
        state.excluded_conditional_player_ids = vec![13];
        state.half_exclusion_increment_ids = vec![13];
        let restored = DraftState::from_snapshot(state.to_snapshot());
        assert_eq!(restored, state);
    }

    #[test]
    fn legacy_draft_state_uses_half_increments_as_conditional_metadata() {
        let mut state = DraftState::with_session(123, LobbyKind::LowSkill, 7);
        state.half_exclusion_increment_ids = vec![13];
        let mut legacy = serde_json::to_value(state.to_snapshot()).expect("serialize draft");
        legacy
            .as_object_mut()
            .expect("draft snapshot object")
            .remove("excluded_conditional_player_ids");
        let restored = DraftStateSnapshot::from_json(
            &serde_json::to_string(&legacy).expect("serialize legacy draft"),
        )
        .expect("deserialize legacy draft")
        .into_state();

        assert!(restored.excluded_conditional_player_ids.is_empty());
        assert_eq!(restored.conditional_exclusion_ids(), [13]);
    }

    #[test]
    fn authoritative_draft_state_round_trips_every_field_through_sqlite() {
        let fixture = NamedTempFile::new().expect("create draft persistence fixture");
        crate::test_support::copy_migrated_database(fixture.path())
            .expect("copy draft persistence fixture");
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
        crate::test_support::copy_migrated_database(fixture.path())
            .expect("copy malformed-row fixture");
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
    fn finalization_plan_freezes_hash_timestamps_destinations_and_delivery_keys() {
        let pending_payload = "{\n  \"future_pending\": true\n}";
        let plan = DraftFinalizationPlan::new(
            TEST_GUILD_ID,
            9,
            pending_payload,
            1_700_000_000,
            1_700_000_000,
            1_700_000_600,
            true,
            LobbyKind::LowSkill,
            Some(10),
            Some(20),
            Some(30),
            Some(40),
            Some(50),
        )
        .expect("build finalization plan");
        assert_eq!(plan.completion_key, format!("draft:{TEST_GUILD_ID}:9"));
        assert_eq!(
            plan.pending_payload_sha256,
            draft_pending_payload_sha256(pending_payload)
        );
        assert_eq!(plan.started_at, 1_700_000_000);
        assert_eq!(plan.shuffle_timestamp, 1_700_000_000);
        assert_eq!(plan.bet_lock_until, 1_700_000_600);
        assert!(plan.is_bomb_pot);
        assert_eq!(plan.lobby_kind, LobbyKind::LowSkill);
        assert_eq!(plan.source.channel_id, Some(10));
        assert_eq!(plan.source.message_id, Some(20));
        assert_eq!(plan.lobby.channel_id, Some(30));
        assert_eq!(plan.origin.channel_id, Some(40));
        assert_eq!(plan.thread_embed.channel_id, Some(50));
        assert_eq!(plan.thread_ping.channel_id, Some(50));
        for (name, delivery) in [
            ("source", &plan.source),
            ("lobby", &plan.lobby),
            ("origin", &plan.origin),
            ("thread-embed", &plan.thread_embed),
            ("thread-ping", &plan.thread_ping),
        ] {
            assert_eq!(
                delivery.delivery_key,
                format!("draft:{TEST_GUILD_ID}:9:{name}")
            );
        }

        let mut raw: serde_json::Value = serde_json::from_str(
            &plan
                .to_json(pending_payload)
                .expect("encode finalization plan"),
        )
        .expect("decode finalization plan JSON");
        raw["future_plan"] = json!({"keep": [1, 2, 3]});
        raw["source"]["future_delivery"] = json!("preserved");
        let decoded = DraftFinalizationPlan::from_json(&raw.to_string())
            .expect("decode forward-compatible plan");
        let reencoded: serde_json::Value = serde_json::from_str(
            &decoded
                .to_json(pending_payload)
                .expect("reencode forward-compatible plan"),
        )
        .expect("decode reencoded plan");
        assert_eq!(reencoded["future_plan"], json!({"keep": [1, 2, 3]}));
        assert_eq!(reencoded["source"]["future_delivery"], json!("preserved"));
        assert!(matches!(
            decoded.to_json("{\"different\":true}"),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));
    }

    #[test]
    fn finalization_plan_matches_live_destination_suppression_and_deduplication() {
        let source_only = DraftFinalizationPlan::new(
            TEST_GUILD_ID,
            9,
            "{}",
            100,
            100,
            200,
            false,
            LobbyKind::Open,
            Some(10),
            Some(20),
            Some(10),
            Some(10),
            None,
        )
        .expect("build source-suppressed plan");
        assert_eq!(source_only.source.channel_id, Some(10));
        assert_eq!(source_only.lobby.channel_id, None);
        assert_eq!(source_only.origin.channel_id, None);

        let deduplicated = DraftFinalizationPlan::new(
            TEST_GUILD_ID,
            9,
            "{}",
            100,
            100,
            200,
            false,
            LobbyKind::Open,
            Some(10),
            Some(20),
            Some(30),
            Some(30),
            None,
        )
        .expect("build destination-deduplicated plan");
        assert_eq!(deduplicated.lobby.channel_id, Some(30));
        assert_eq!(deduplicated.origin.channel_id, None);

        let distinct_origin = DraftFinalizationPlan::new(
            TEST_GUILD_ID,
            9,
            "{}",
            100,
            100,
            200,
            false,
            LobbyKind::Open,
            Some(10),
            Some(20),
            Some(10),
            Some(40),
            None,
        )
        .expect("build distinct-origin plan");
        assert_eq!(distinct_origin.lobby.channel_id, None);
        assert_eq!(distinct_origin.origin.channel_id, Some(40));

        let mut invalid: serde_json::Value =
            serde_json::to_value(&deduplicated).expect("encode destination-deduplicated plan");
        invalid["lobby"]["channel_id"] = json!(10);
        assert!(matches!(
            DraftFinalizationPlan::from_json(&invalid.to_string()),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));
        invalid["lobby"]["channel_id"] = json!(30);
        invalid["origin"]["channel_id"] = json!(30);
        assert!(matches!(
            DraftFinalizationPlan::from_json(&invalid.to_string()),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));
    }

    /// A policy that passes `DraftFinancialSetupPolicy::validate`, for tests
    /// that only need a seed the link boundary will accept.
    fn linkable_financial_policy() -> DraftFinancialSetupPolicy {
        DraftFinancialSetupPolicy {
            schema_version: cama_db::draft_financial_setup::DRAFT_FINANCIAL_SETUP_PLAN_VERSION,
            game_date: "2026-08-14".to_owned(),
            betting_mode: "pool".to_owned(),
            seed_max_amount: 0,
            first_game_daily_amount: 0,
            auto_blind_enabled: true,
            normal_blind_threshold: 0,
            normal_blind_percentage_bits: format!("{:016x}", 0.1_f64.to_bits()),
            bomb_blind_percentage_bits: format!("{:016x}", 0.15_f64.to_bits()),
            bomb_ante: 10,
            max_debt: 100,
            spectator_enabled: false,
            spectator_total_count: 0,
            spectator_top_count: 0,
            spectator_base_percentage_bits: format!("{:016x}", 0.01_f64.to_bits()),
            spectator_top_percentage_bits: format!("{:016x}", 0.02_f64.to_bits()),
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn a_legacy_typed_plan_is_refused_at_the_link_boundary() {
        let fixture = NamedTempFile::new().expect("create typed-link fixture");
        crate::test_support::copy_migrated_database(fixture.path())
            .expect("copy typed-link fixture");
        let persistence = SqliteDraftStatePersistence::new(fixture.path());
        let mut state = DraftState::with_session(TEST_GUILD_ID, LobbyKind::Open, 0);
        state.phase = DraftPhase::Complete;
        let mut envelope = state.to_snapshot().envelope(1);
        envelope.finalizing = true;
        envelope.deadline_at = None;
        let created = persistence
            .create_envelope(&envelope)
            .expect("create fenced envelope");
        let pending_payload = json!({
            "shuffle_timestamp": 1_700_000_000_i64,
            "is_bomb_pot": false,
            "future_pending": {"keep": true}
        })
        .to_string();
        let plan = DraftFinalizationPlan::new(
            TEST_GUILD_ID,
            created.session_id,
            &pending_payload,
            1_700_000_000,
            1_700_000_000,
            1_700_000_600,
            false,
            LobbyKind::Open,
            Some(10),
            Some(20),
            Some(30),
            Some(40),
            Some(50),
        )
        .expect("build typed plan")
        .to_json(&pending_payload)
        .expect("encode typed plan");

        // A legacy plan carries no financial setup, so `run_leased_financial_setup`
        // would refuse it and the job would pin itself at `linked` forever,
        // reappearing in every recovery pass. The link boundary refuses it first.
        let error = persistence
            .link_finalizing_pending_match(
                TEST_GUILD_ID,
                created.session_id,
                created.revision,
                &pending_payload,
                &plan,
            )
            .expect_err("a legacy plan is not linkable");
        assert!(matches!(error, DraftPersistenceError::Serialization { .. }));
        let connection = Connection::open(fixture.path()).expect("reopen typed-link fixture");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count pending matches"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count finalization jobs"),
            0
        );
        assert_eq!(
            persistence
                .load_all()
                .expect("reload untouched envelope")
                .len(),
            1
        );
    }

    #[test]
    fn sqlite_adapter_v2_link_returns_the_frozen_financial_plan() {
        let fixture = NamedTempFile::new().expect("create v2 typed-link fixture");
        crate::test_support::copy_migrated_database(fixture.path())
            .expect("copy v2 typed-link fixture");
        let connection = Connection::open(fixture.path()).expect("open v2 typed-link fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable unrelated legacy foreign-key mismatch for fixture setup");
        for discord_id in 1_i64..=10 {
            connection
                .execute(
                    "INSERT INTO players(
                         discord_id,guild_id,discord_username,jopacoin_balance
                     ) VALUES (?1,?2,?3,100)",
                    rusqlite::params![discord_id, TEST_GUILD_ID, format!("player-{discord_id}")],
                )
                .expect("insert v2 financial player");
        }
        drop(connection);

        let persistence = SqliteDraftStatePersistence::new(fixture.path());
        let mut state = DraftState::with_session(TEST_GUILD_ID, LobbyKind::Open, 0);
        state.phase = DraftPhase::Complete;
        let mut envelope = state.to_snapshot().envelope(1);
        envelope.finalizing = true;
        envelope.deadline_at = None;
        let created = persistence
            .create_envelope(&envelope)
            .expect("create v2 fenced envelope");
        let pending_payload = json!({
            "radiant_team_ids": [1, 2, 3, 4, 5],
            "dire_team_ids": [6, 7, 8, 9, 10],
            "shuffle_timestamp": 1_700_000_000_i64,
            "bet_lock_until": 1_700_000_600_i64,
            "betting_mode": "pool",
            "is_bomb_pot": false,
            "lobby_kind": "open",
            "future_pending": {"keep": true}
        })
        .to_string();
        let policy = DraftFinancialSetupPolicy {
            schema_version: cama_db::draft_financial_setup::DRAFT_FINANCIAL_SETUP_PLAN_VERSION,
            game_date: "2026-08-14".to_owned(),
            betting_mode: "pool".to_owned(),
            seed_max_amount: 0,
            first_game_daily_amount: 0,
            auto_blind_enabled: true,
            normal_blind_threshold: 0,
            normal_blind_percentage_bits: format!("{:016x}", 0.1_f64.to_bits()),
            bomb_blind_percentage_bits: format!("{:016x}", 0.15_f64.to_bits()),
            bomb_ante: 10,
            max_debt: 100,
            spectator_enabled: false,
            spectator_total_count: 0,
            spectator_top_count: 0,
            spectator_base_percentage_bits: format!("{:016x}", 0.01_f64.to_bits()),
            spectator_top_percentage_bits: format!("{:016x}", 0.02_f64.to_bits()),
            extensions: BTreeMap::from([("future_policy".to_owned(), json!("keep"))]),
        };
        let mut seed = DraftFinalizationPlanSeed::new(
            TEST_GUILD_ID,
            created.session_id,
            &pending_payload,
            1_700_000_000,
            1_700_000_000,
            1_700_000_600,
            false,
            LobbyKind::Open,
            Some(10),
            Some(20),
            Some(30),
            Some(40),
            Some(50),
            policy,
        )
        .expect("build v2 plan seed");
        seed.extensions
            .insert("future_plan".to_owned(), json!({"keep": [1, 2, 3]}));
        let seed_json = seed.to_json(&pending_payload).expect("encode v2 plan seed");
        let linked = persistence
            .link_finalizing_pending_match_v2(
                TEST_GUILD_ID,
                created.session_id,
                created.revision,
                &pending_payload,
                &seed_json,
            )
            .expect("atomically link v2 financial plan");
        let plan = DraftFinalizationPlan::from_json(&linked.plan_json)
            .expect("decode complete frozen v2 plan");
        plan.validate(&pending_payload)
            .expect("validate complete frozen v2 plan");
        assert_eq!(plan.schema_version, DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION);
        assert_eq!(plan.extensions["future_plan"], json!({"keep": [1, 2, 3]}));
        let financial = plan
            .financial_setup
            .as_ref()
            .expect("frozen financial setup");
        assert_eq!(financial.pending_match_id, linked.pending_match_id);
        assert_eq!(financial.policy.extensions["future_policy"], json!("keep"));
        assert_eq!(
            financial.effects[0].effect_kind,
            DraftFinancialEffectKind::Seed
        );
        assert_eq!(
            financial.effects[1].effect_kind,
            DraftFinancialEffectKind::Blind
        );
        assert_eq!(financial.effects[1].intended["amount"], json!(10));

        let legacy = DraftFinalizationPlan::new(
            TEST_GUILD_ID,
            created.session_id,
            &pending_payload,
            1_700_000_000,
            1_700_000_000,
            1_700_000_600,
            false,
            LobbyKind::Open,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("build legacy delivery-only plan");
        assert_eq!(
            legacy.schema_version,
            cama_db::draft_finalization::DRAFT_FINALIZATION_LEGACY_PLAN_VERSION
        );
        assert!(legacy.financial_setup.is_none());

        let mut missing_financial = serde_json::to_value(&plan).expect("encode frozen v2 plan");
        missing_financial
            .as_object_mut()
            .expect("v2 plan object")
            .remove("financial_setup");
        assert!(matches!(
            DraftFinalizationPlan::from_json(&missing_financial.to_string()),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));
        let mut mismatched_identity =
            serde_json::to_value(&plan).expect("encode v2 identity fixture");
        let old_prefix = format!("draft:{TEST_GUILD_ID}:{}", created.session_id);
        let new_prefix = format!("draft:{}:{}", TEST_GUILD_ID + 1, created.session_id);
        for effect in mismatched_identity["financial_setup"]["effects"]
            .as_array_mut()
            .expect("v2 financial effects")
        {
            effect["intended"]["guild_id"] = json!(TEST_GUILD_ID + 1);
            let key = effect["effect_key"]
                .as_str()
                .expect("v2 effect key")
                .replacen(&old_prefix, &new_prefix, 1);
            effect["effect_key"] = json!(key);
        }
        serde_json::from_value::<DraftFinancialSetupPlan>(
            mismatched_identity["financial_setup"].clone(),
        )
        .expect("decode internally consistent mismatched financial plan")
        .validate()
        .expect("nested validation alone cannot see the top-level identity");
        assert!(matches!(
            DraftFinalizationPlan::from_json(&mismatched_identity.to_string()),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));

        let setup = persistence
            .run_finalizing_financial_setup(
                TEST_GUILD_ID,
                created.session_id,
                linked.pending_match_id,
                1_700_000_100,
            )
            .expect("run frozen financial setup");
        assert!(!setup.replayed);
        let setup_job = persistence
            .finalization_job(TEST_GUILD_ID, created.session_id, linked.pending_match_id)
            .expect("load setup-complete job")
            .expect("setup-complete job");
        let owner = draft_finalization_process_owner(TEST_GUILD_ID, created.session_id);
        let published = persistence
            .advance_finalization_job(
                TEST_GUILD_ID,
                created.session_id,
                linked.pending_match_id,
                setup_job.revision,
                DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                "{}",
                &owner,
                1_700_000_101,
            )
            .expect("advance publication stage");
        assert_eq!(
            published.stage,
            DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE
        );
        persistence
            .complete_finalization_job(
                TEST_GUILD_ID,
                created.session_id,
                linked.pending_match_id,
                linked.envelope.revision,
                1_700_000_101,
            )
            .expect("complete published finalization");
        assert!(
            persistence
                .load_all()
                .expect("reload terminal envelopes")
                .is_empty()
        );
        let job = cama_db::draft_finalization::DraftFinalizationRepository::new(fixture.path())
            .job(&linked.completion_key)
            .expect("reload terminal v2 job")
            .expect("terminal v2 job");
        assert_eq!(
            job.stage,
            cama_db::draft_finalization::DRAFT_FINALIZATION_COMPLETE_STAGE
        );
    }

    #[test]
    fn atomic_pending_link_rejects_typed_envelope_before_committing() {
        let fixture = NamedTempFile::new().expect("create malformed-link fixture");
        crate::test_support::copy_migrated_database(fixture.path())
            .expect("copy malformed-link fixture");
        let malformed = json!({
            "schema_version": DRAFT_STATE_SCHEMA_VERSION,
            "guild_id": TEST_GUILD_ID,
            "session_id": 9,
            "revision": 1,
            "active": true,
            "finalizing": true,
            "pending_match_id": null,
            "state": {
                "guild_id": TEST_GUILD_ID,
                "session_id": 9,
                "phase": "complete"
            }
        })
        .to_string();
        let connection = Connection::open(fixture.path()).expect("open malformed-link fixture");
        connection
            .execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,?2,?3)",
                rusqlite::params![
                    TEST_GUILD_ID,
                    cama_db::draft_state::DRAFT_STATE_KEY,
                    malformed
                ],
            )
            .expect("insert malformed finalization envelope");
        drop(connection);

        let persistence = SqliteDraftStatePersistence::new(fixture.path());
        // A linkable seed, so the malformed envelope is what rejects the link.
        let pending_payload =
            json!({"shuffle_timestamp": 1_700_000_000_i64, "is_bomb_pot": false}).to_string();
        let seed = DraftFinalizationPlanSeed::new(
            TEST_GUILD_ID,
            9,
            &pending_payload,
            1_700_000_000,
            1_700_000_000,
            1_700_000_600,
            false,
            LobbyKind::Open,
            Some(10),
            Some(20),
            Some(30),
            Some(40),
            Some(50),
            linkable_financial_policy(),
        )
        .expect("build finalization plan seed")
        .to_json(&pending_payload)
        .expect("encode finalization plan seed");
        assert!(matches!(
            persistence.link_finalizing_pending_match_v2(
                TEST_GUILD_ID,
                9,
                1,
                &pending_payload,
                &seed,
            ),
            Err(DraftPersistenceError::InvalidEnvelope { .. })
        ));
        let connection = Connection::open(fixture.path()).expect("reopen malformed-link fixture");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count pending matches"),
            0
        );
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
    fn production_coinflip_is_random_rather_than_alternating() {
        // The default service is what the runtime composes, so its flips must
        // not be predictable from the previous flip.
        let mut service = DraftService::new();
        let flips = (0..200)
            .map(|_| service.coinflip(100, 200))
            .collect::<Vec<_>>();

        assert!(flips.contains(&100) && flips.contains(&200));
        assert!(
            flips.windows(2).any(|pair| pair[0] == pair[1]),
            "a strictly alternating coin never repeats a winner"
        );
    }

    #[test]
    fn alternating_coin_remains_available_for_deterministic_tests() {
        let mut service = DraftService::with_coin(AlternatingDraftCoin::default());

        assert_eq!(
            (0..4)
                .map(|_| service.coinflip(100, 200))
                .collect::<Vec<_>>(),
            vec![100, 200, 100, 200]
        );
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
    fn test_sixteen_players_continue_to_normal_shuffle() {
        let player_ids = (1..=16).collect::<Vec<_>>();
        let result = route_large_lobby_shuffle(&player_ids, &BTreeSet::new());
        assert!(!result.draft_called);
        assert!(result.response.is_none());
        let blocked = route_large_lobby_shuffle(&player_ids, &BTreeSet::from([16]));
        assert!(!blocked.draft_called);
        assert!(blocked.response.unwrap().ephemeral);
    }
}
