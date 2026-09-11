//! Durable pending-match persistence for the live match runtime.
//!
//! The Python-owned schema stores the complete pending match as JSON in
//! `pending_matches.payload`.  This adapter opens only an already-migrated
//! database through [`crate::open_runtime_connection`]; production code in
//! this module never creates or alters schema.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cama_domain::dota_hosting::{DotaHostingOptions, HostingMode};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::dota_session_repository::DotaSessionPhase;
use crate::open_runtime_connection;
pub use cama_db_core::dota_host_routing::DotaHostRouting;

/// One result/abort submission embedded in a pending-match payload.
///
/// The flattened map deliberately retains fields added by a newer runtime so
/// an older Rust binary can read and rewrite the document without erasing
/// them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingMatchSubmission {
    pub result: Option<String>,
    pub is_admin: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Typed form of `pending_matches.payload`.
///
/// Missing fields use the same defaults as Python's `PendingMatchState` and
/// unknown fields survive round trips through `extra`.  The row primary key
/// intentionally lives on [`PendingMatchRecord`] and is never emitted into
/// the JSON payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingMatchState {
    pub radiant_team_ids: Vec<i64>,
    pub dire_team_ids: Vec<i64>,
    pub excluded_player_ids: Vec<i64>,
    pub excluded_conditional_player_ids: Vec<i64>,

    pub radiant_roles: Vec<String>,
    pub dire_roles: Vec<String>,
    pub radiant_value: f64,
    pub dire_value: f64,
    pub value_diff: f64,
    pub first_pick_team: Option<String>,

    pub record_submissions: BTreeMap<i64, PendingMatchSubmission>,

    pub shuffle_timestamp: Option<i64>,
    pub lock_time: Option<i64>,
    pub bet_lock_until: Option<i64>,

    pub betting_mode: String,
    pub is_bomb_pot: bool,
    pub is_openskill_shuffle: bool,
    pub is_draft: bool,
    pub balancing_rating_system: String,
    pub lobby_kind: Option<String>,

    pub bet_seed_reserved: i64,
    pub bet_seed_radiant: i64,
    pub bet_seed_dire: i64,
    pub bet_seed_bonus: i64,
    pub first_game_pool_reserved: i64,

    pub blind_bets_result: Option<Value>,

    pub effective_avoid_ids: Vec<i64>,
    pub effective_deal_ids: Vec<i64>,
    pub exclusion_updates_deferred: bool,
    pub full_exclusion_increment_ids: Vec<i64>,
    pub half_exclusion_increment_ids: Vec<i64>,

    pub shuffle_channel_id: Option<i64>,
    pub shuffle_message_id: Option<i64>,
    pub shuffle_message_jump_url: Option<String>,
    pub cmd_shuffle_channel_id: Option<i64>,
    pub cmd_shuffle_message_id: Option<i64>,
    pub thread_shuffle_message_id: Option<i64>,
    pub thread_shuffle_thread_id: Option<i64>,
    pub origin_channel_id: Option<i64>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for PendingMatchState {
    fn default() -> Self {
        Self {
            radiant_team_ids: Vec::new(),
            dire_team_ids: Vec::new(),
            excluded_player_ids: Vec::new(),
            excluded_conditional_player_ids: Vec::new(),
            radiant_roles: Vec::new(),
            dire_roles: Vec::new(),
            radiant_value: 0.0,
            dire_value: 0.0,
            value_diff: 0.0,
            first_pick_team: None,
            record_submissions: BTreeMap::new(),
            shuffle_timestamp: None,
            lock_time: None,
            bet_lock_until: None,
            betting_mode: "pool".to_owned(),
            is_bomb_pot: false,
            is_openskill_shuffle: false,
            is_draft: false,
            balancing_rating_system: "glicko".to_owned(),
            lobby_kind: None,
            bet_seed_reserved: 0,
            bet_seed_radiant: 0,
            bet_seed_dire: 0,
            bet_seed_bonus: 0,
            first_game_pool_reserved: 0,
            blind_bets_result: None,
            effective_avoid_ids: Vec::new(),
            effective_deal_ids: Vec::new(),
            exclusion_updates_deferred: false,
            full_exclusion_increment_ids: Vec::new(),
            half_exclusion_increment_ids: Vec::new(),
            shuffle_channel_id: None,
            shuffle_message_id: None,
            shuffle_message_jump_url: None,
            cmd_shuffle_channel_id: None,
            cmd_shuffle_message_id: None,
            thread_shuffle_message_id: None,
            thread_shuffle_thread_id: None,
            origin_channel_id: None,
            extra: BTreeMap::new(),
        }
    }
}

impl PendingMatchState {
    /// Whether Dota lobby automation has adopted this match and owns the
    /// point at which the betting window is closed.
    ///
    /// The closed marker deliberately wins over the adoption marker.  Keeping
    /// that precedence here gives runtime recovery, reminders, and callers
    /// that load the typed payload the same answer after a restart.
    #[must_use]
    pub fn hosted_betting_managed(&self) -> bool {
        self.extra
            .get(DOTA_HOSTED_BETTING_MARKER)
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && !self.betting_closed()
    }

    /// Whether this pending match still accepts a wager at `now_unix`.
    ///
    /// Hosted admission requires a fresh owned-lobby observation or a bounded
    /// explicit administrator extension. Legacy manual matches retain their
    /// original timestamp policy.
    #[must_use]
    pub fn betting_open(&self, now_unix: i64) -> bool {
        if self
            .extra
            .get("draft_setup_complete")
            .and_then(Value::as_bool)
            == Some(false)
        {
            return false;
        }
        if self
            .extra
            .get("dota_betting_suspended")
            .and_then(Value::as_bool)
            == Some(true)
        {
            return false;
        }
        if self
            .extra
            .get("shuffle_setup_complete")
            .and_then(Value::as_bool)
            == Some(false)
        {
            return false;
        }
        if self.betting_closed() {
            // Dota's automatic close is terminal for the ordinary timed
            // window.  An administrator may explicitly reopen that window
            // by extending it; the persisted extension deadline is the only
            // authority while the closed marker is present.
            return self
                .betting_extension_until()
                .is_some_and(|extension_until| now_unix < extension_until);
        }
        if self.hosted_betting_managed() {
            return self
                .betting_extension_until()
                .is_some_and(|until| now_unix < until)
                || self
                    .extra
                    .get(DOTA_HOSTED_BETTING_OBSERVED_AT)
                    .and_then(Value::as_i64)
                    .is_some_and(|observed| {
                        observed <= now_unix
                            && now_unix.saturating_sub(observed)
                                < DOTA_HOSTED_BETTING_OBSERVATION_TTL_SECONDS
                    });
        }
        self.bet_lock_until
            .is_some_and(|lock_until| lock_until != 0 && now_unix < lock_until)
    }

    #[must_use]
    pub fn betting_closed(&self) -> bool {
        self.extra
            .get(DOTA_BETTING_CLOSED_MARKER)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Explicit administrator deadline persisted by a Dota betting-window
    /// extension.  Non-positive and malformed values are ignored so a bad
    /// payload cannot accidentally reopen an automatically closed match.
    #[must_use]
    pub fn betting_extension_until(&self) -> Option<i64> {
        self.extra
            .get(DOTA_BETTING_EXTENDED_UNTIL)
            .and_then(Value::as_i64)
            .filter(|extension_until| *extension_until > 0)
    }

    /// Actual match participants, excluding players left out of the game.
    #[must_use]
    pub fn participant_ids(&self) -> BTreeSet<i64> {
        self.radiant_team_ids
            .iter()
            .chain(&self.dire_team_ids)
            .copied()
            .collect()
    }

    /// Everyone from the shuffled lobby who may vote to abort.
    #[must_use]
    pub fn full_lobby_player_ids(&self) -> BTreeSet<i64> {
        self.radiant_team_ids
            .iter()
            .chain(&self.dire_team_ids)
            .chain(&self.excluded_player_ids)
            .chain(&self.excluded_conditional_player_ids)
            .copied()
            .collect()
    }

    #[must_use]
    pub fn contains_participant(&self, player_id: i64) -> bool {
        self.radiant_team_ids.contains(&player_id) || self.dire_team_ids.contains(&player_id)
    }

    #[must_use]
    pub fn contains_full_lobby_player(&self, player_id: i64) -> bool {
        self.contains_participant(player_id)
            || self.excluded_player_ids.contains(&player_id)
            || self.excluded_conditional_player_ids.contains(&player_id)
    }
}

/// Payload key set by [`PendingMatchRepository::begin_hosted_betting`] after
/// the bot has durably adopted a Dota lobby.
pub const DOTA_HOSTED_BETTING_OBSERVED_AT: &str = "dota_hosted_betting_observed_at";
pub use cama_domain::dota_hosting::BETTING_OBSERVATION_TTL_SECONDS as DOTA_HOSTED_BETTING_OBSERVATION_TTL_SECONDS;

pub const DOTA_HOSTED_BETTING_MARKER: &str = "dota_hosted_betting";

/// Payload key set by [`PendingMatchRepository::close_betting_now`] when
/// betting has reached its terminal pregame boundary.
pub const DOTA_BETTING_CLOSED_MARKER: &str = "dota_betting_closed";

/// Payload key set by [`PendingMatchRepository::extend_betting_atomic`] for
/// Dota-managed windows.  Unlike the historical `bet_lock_until`, this
/// deadline remains authoritative after an automatic close so an explicit
/// administrator extension can reopen the window until this exact time.
pub const DOTA_BETTING_EXTENDED_UNTIL: &str = "dota_betting_extended_until";

/// Optional audit timestamp for the first successful hosted-betting adoption.
pub const DOTA_HOSTED_BETTING_STARTED_AT: &str = "dota_hosted_betting_started_at";

/// Optional audit timestamp for a cancellation that releases hosted betting.
pub const DOTA_HOSTED_BETTING_RELEASED_AT: &str = "dota_hosted_betting_released_at";

/// Row metadata kept outside the JSON payload.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingMatchRecord {
    pub pending_match_id: i64,
    pub guild_id: i64,
    pub state: PendingMatchState,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingBettingExtension {
    pub pending_match: PendingMatchRecord,
    pub old_lock_until: i64,
    pub new_lock_until: i64,
}

/// Result of atomically closing the betting window for a Dota-managed match.
///
/// `old_lock_until` is `None` when a legacy payload had no usable betting
/// lock.  The payload marker still closes such a row; an explicit administrator
/// extension may reopen it only until its persisted Dota extension deadline.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingBettingClose {
    pub pending_match: PendingMatchRecord,
    pub old_lock_until: Option<i64>,
    pub new_lock_until: i64,
}

#[derive(Debug, Error)]
pub enum PendingMatchRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("pending match {pending_match_id} contains malformed JSON: {message}")]
    MalformedPayload {
        pending_match_id: i64,
        message: String,
    },
    #[error("failed to encode pending-match JSON: {0}")]
    EncodePayload(String),
    #[error("pending match {0} was not found")]
    PendingMatchNotFound(i64),
    #[error("pending match {0} was already recorded as a completed match")]
    MatchAlreadyRecorded(i64),
    #[error("a participant already belongs to pending match {0}")]
    ParticipantAlreadyPending(i64),
    #[error("pending match {0} setup is incomplete")]
    SetupIncomplete(i64),
    #[error("pending match {0} requires atomic financial abort")]
    FinancialAbortRequired(i64),
    #[error("pending match {0} has no betting window")]
    MissingBettingWindow(i64),
    #[error("betting for pending match {0} was closed by Dota lobby automation")]
    BettingClosedByDotaSession(i64),
    #[error(
        "Dota lobby for pending match {0} has already started; manual hosting cannot replace it"
    )]
    DotaSessionAlreadyStarted(i64),
    #[error("betting extension must be a positive number of seconds")]
    InvalidBettingExtension,
    #[error("integer arithmetic overflow")]
    ArithmeticOverflow,
}

#[derive(Clone, Debug)]
pub struct PendingMatchRepository {
    path: PathBuf,
}

impl PendingMatchRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn configure_dota_host_routing(
        &self,
        routing: Option<DotaHostRouting>,
    ) -> Result<(), PendingMatchRepositoryError> {
        cama_db_core::dota_host_routing::configure(
            &mut open_runtime_connection(&self.path)?,
            routing,
        )
        .map_err(Into::into)
    }

    /// Insert a new row. Multiple pending matches in one guild are allowed.
    pub fn create_pending_match(
        &self,
        guild_id: i64,
        state: &PendingMatchState,
    ) -> Result<PendingMatchRecord, PendingMatchRepositoryError> {
        let mut payload = serde_json::to_value(state).map_err(|e| {
            PendingMatchRepositoryError::MalformedPayload {
                pending_match_id: 0,
                message: e.to_string(),
            }
        })?;
        if let Some(object) = payload.as_object_mut() {
            object.remove("pending_match_id");
        }
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if state.extra.contains_key("shuffle_setup_complete") {
            let participants = state
                .radiant_team_ids
                .iter()
                .chain(&state.dire_team_ids)
                .copied()
                .collect::<BTreeSet<_>>();
            for raw in query_all_raw(&transaction, guild_id)? {
                let existing = decode_raw(raw)?;
                if existing
                    .state
                    .radiant_team_ids
                    .iter()
                    .chain(&existing.state.dire_team_ids)
                    .any(|id| participants.contains(id))
                {
                    return Err(PendingMatchRepositoryError::ParticipantAlreadyPending(
                        existing.pending_match_id,
                    ));
                }
            }
        }
        cama_db_core::dota_host_routing::route_pending(&transaction, guild_id, &mut payload)?;
        let encoded = payload.to_string();
        transaction.execute(
            "INSERT INTO pending_matches (guild_id,payload,updated_at)
             VALUES (?1,?2,CURRENT_TIMESTAMP)",
            params![guild_id, encoded],
        )?;
        let pending_match_id = transaction.last_insert_rowid();
        let raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let record = decode_raw(raw)?;
        transaction.commit()?;
        Ok(record)
    }

    /// Load one explicitly selected row, always guarded by its guild.
    pub fn pending_match(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        query_raw(&connection, guild_id, pending_match_id)?
            .map(decode_raw)
            .transpose()
    }

    /// Backward-compatible selection: return a row only when the guild has
    /// exactly one pending match.
    pub fn single_pending_match(
        &self,
        guild_id: i64,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT pending_match_id,guild_id,payload,created_at,updated_at
               FROM pending_matches
              WHERE guild_id=?1
              ORDER BY created_at,pending_match_id
              LIMIT 2",
        )?;
        let rows = statement
            .query_map(params![guild_id], raw_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() != 1 {
            return Ok(None);
        }
        rows.into_iter().next().map(decode_raw).transpose()
    }

    /// Load every pending match in stable creation order.
    pub fn pending_matches(
        &self,
        guild_id: i64,
    ) -> Result<Vec<PendingMatchRecord>, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        query_all_raw(&connection, guild_id)?
            .into_iter()
            .map(decode_raw)
            .collect()
    }

    /// Bounded setup and committed-result recovery queue across guilds.
    pub fn unfinished_shuffle_setups(
        &self,
    ) -> Result<Vec<PendingMatchRecord>, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT pending_match_id,guild_id,payload,created_at,updated_at
             FROM pending_matches
             WHERE (json_valid(payload) AND json_extract(payload,'$.shuffle_setup_complete')=0)
                OR EXISTS(SELECT 1 FROM matches WHERE matches.guild_id=pending_matches.guild_id
                    AND matches.pending_match_id=pending_matches.pending_match_id)
             ORDER BY updated_at,pending_match_id LIMIT 500",
        )?;
        let rows = statement
            .query_map([], raw_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(decode_raw).collect()
    }

    /// Select the oldest pending match in which the player is on a team.
    pub fn pending_match_for_participant(
        &self,
        guild_id: i64,
        player_id: i64,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        Ok(self
            .pending_matches(guild_id)?
            .into_iter()
            .find(|pending| pending.state.contains_participant(player_id)))
    }

    /// Select the oldest pending match whose complete shuffled lobby contains
    /// the player. This additionally includes both exclusion lists.
    pub fn pending_match_for_full_lobby_player(
        &self,
        guild_id: i64,
        player_id: i64,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        Ok(self
            .pending_matches(guild_id)?
            .into_iter()
            .find(|pending| pending.state.contains_full_lobby_player(player_id)))
    }

    /// All actual participants across the guild's pending matches.
    pub fn all_pending_player_ids(
        &self,
        guild_id: i64,
    ) -> Result<BTreeSet<i64>, PendingMatchRepositoryError> {
        Ok(self
            .pending_matches(guild_id)?
            .iter()
            .flat_map(|pending| pending.state.participant_ids())
            .collect())
    }

    /// Replace the complete JSON document under a strict guild guard.
    ///
    /// Unknown fields loaded into `state.extra` remain present in the encoded
    /// document. Returns `false` for a missing or cross-guild row.
    pub fn update_pending_match(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        state: &PendingMatchState,
    ) -> Result<bool, PendingMatchRepositoryError> {
        let encoded = encode_state(state)?;
        let connection = open_runtime_connection(&self.path)?;
        let changed = connection.execute(
            "UPDATE pending_matches
                SET payload=?1,updated_at=CURRENT_TIMESTAMP
              WHERE guild_id=?2 AND pending_match_id=?3",
            params![encoded, guild_id, pending_match_id],
        )?;
        Ok(changed == 1)
    }

    /// Freeze manual hosting and request cleanup of an unlaunched bot lobby
    /// atomically. The session revision invalidates a worker's stale snapshot.
    pub fn request_manual_dota_hosting(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<PendingMatchRecord, PendingMatchRepositoryError> {
        let malformed = |message: String| PendingMatchRepositoryError::MalformedPayload {
            pending_match_id,
            message,
        };
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let mut payload = decode_value(pending_match_id, &raw.payload)?;
        let pending = decode_raw(raw)?;
        let options = DotaHostingOptions::from_extra(&pending.state.extra)
            .map_err(&malformed)?
            .merged(&DotaHostingOptions {
                hosting: Some(HostingMode::Manual),
                ..Default::default()
            });

        let session: Option<(String, Option<String>, String, i64)> = transaction.query_row(
            "SELECT phase,valve_match_id,payload,revision FROM dota_sessions WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?;
        if let Some((phase, valve_match_id, payload, revision)) = session {
            let phase = DotaSessionPhase::parse(&phase)
                .ok_or_else(|| malformed("Invalid Dota session phase.".into()))?;
            if phase.is_active() {
                let mut payload: Value = serde_json::from_str(&payload)
                    .map_err(|error| malformed(format!("Invalid Dota session payload: {error}")))?;
                let object = payload
                    .as_object_mut()
                    .ok_or_else(|| malformed("Dota session payload must be an object.".into()))?;
                if valve_match_id.is_some()
                    || object
                        .get("launch_requested_at")
                        .is_some_and(|value| !value.is_null())
                    || matches!(
                        phase,
                        DotaSessionPhase::Launching
                            | DotaSessionPhase::Running
                            | DotaSessionPhase::Finishing
                    )
                {
                    return Err(PendingMatchRepositoryError::DotaSessionAlreadyStarted(
                        pending_match_id,
                    ));
                }
                object.insert("cancel_requested".into(), Value::Bool(true));
                let next_revision = revision
                    .checked_add(1)
                    .ok_or(PendingMatchRepositoryError::ArithmeticOverflow)?;
                transaction.execute(
                    "UPDATE dota_sessions SET payload=?1,revision=?2,updated_at=unixepoch() WHERE guild_id=?3 AND pending_match_id=?4",
                    params![payload.to_string(), next_revision, guild_id, pending_match_id],
                )?;
            }
        }
        let object = payload
            .as_object_mut()
            .ok_or_else(|| malformed("Pending match payload must be an object.".into()))?;
        object.insert(
            "dota_hosting".into(),
            serde_json::to_value(options)
                .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))?,
        );
        // Discovery adopts betting while a match is still queued for the
        // host. It therefore needs release even when no session exists yet.
        let now_unix = transaction.query_row("SELECT unixepoch()", [], |row| row.get(0))?;
        release_hosted_betting_payload(object, now_unix);
        transaction.execute(
            "UPDATE pending_matches SET payload=?1,updated_at=CURRENT_TIMESTAMP WHERE guild_id=?2 AND pending_match_id=?3",
            params![payload.to_string(), guild_id, pending_match_id],
        )?;
        let updated = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let updated = decode_raw(updated)?;
        transaction.commit()?;
        Ok(updated)
    }

    /// Atomically reload, mutate, and persist one pending-match document.
    ///
    /// The immediate writer lock is acquired before the row is read. This
    /// serializes the mutation with voting's payload CAS and other SQLite
    /// writers, preventing a metadata refresh from replacing a newly cast
    /// vote. Unknown JSON fields remain present through `state.extra`.
    pub fn mutate_pending_match<R>(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        mutate: impl FnOnce(&mut PendingMatchState) -> R,
    ) -> Result<Option<(PendingMatchRecord, R)>, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(raw) = query_raw(&transaction, guild_id, pending_match_id)? else {
            transaction.commit()?;
            return Ok(None);
        };
        let mut pending = decode_raw(raw)?;
        let output = mutate(&mut pending.state);
        let encoded = encode_state(&pending.state)?;
        let changed = transaction.execute(
            "UPDATE pending_matches
                SET payload=?1,updated_at=CURRENT_TIMESTAMP
              WHERE guild_id=?2 AND pending_match_id=?3",
            params![encoded, guild_id, pending_match_id],
        )?;
        if changed != 1 {
            return Err(PendingMatchRepositoryError::PendingMatchNotFound(
                pending_match_id,
            ));
        }
        let updated_raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let updated = decode_raw(updated_raw)?;
        transaction.commit()?;
        Ok(Some((updated, output)))
    }

    /// Delete one explicitly selected row under a strict guild guard.
    pub fn delete_pending_match(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<bool, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Final financial policy, delivery receipts and operator decisions
        // outlive cleanup. First archive wins across retries and corrections.
        transaction.execute(
            "INSERT INTO app_kv(guild_id,key,value)
             SELECT pending_matches.guild_id,'match-finalization:' || matches.match_id,pending_matches.payload
             FROM pending_matches JOIN matches ON matches.guild_id=pending_matches.guild_id
                 AND matches.pending_match_id=pending_matches.pending_match_id
             WHERE pending_matches.guild_id=?1 AND pending_matches.pending_match_id=?2
             ON CONFLICT(guild_id,key) DO NOTHING",
            params![guild_id,pending_match_id],
        )?;
        let changed = transaction.execute(
            "DELETE FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Atomically consume one aborted match and credit its participants once.
    ///
    /// A missing pending row is a no-op so retrying a stale abort cannot grant
    /// the exclusion-factor credit more than once. A committed `matches` row
    /// for the same pending identity refuses the abort outright: the record
    /// path already committed durably (its pending-row cleanup may still be in
    /// flight), so a late abort must not stack a spurious exclusion credit on
    /// a completed match or claim its bets were refunded.
    pub fn finalize_abort(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        participant_ids: &[i64],
    ) -> Result<bool, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recorded = transaction
            .query_row(
                "SELECT match_id FROM matches
                 WHERE guild_id=?1 AND pending_match_id=?2",
                params![guild_id, pending_match_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if recorded.is_some() {
            return Err(PendingMatchRepositoryError::MatchAlreadyRecorded(
                pending_match_id,
            ));
        }
        let finances_pending: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM bets WHERE guild_id=?1 AND pending_match_id=?2 AND match_id IS NULL)
                OR EXISTS(SELECT 1 FROM first_game_pool_claims WHERE guild_id=?1 AND pending_match_id=?2 AND settled=0)
                OR EXISTS(SELECT 1 FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2
                    AND (COALESCE(json_extract(payload,'$.bet_seed_reserved'),0)>0
                        OR COALESCE(json_extract(payload,'$.bet_seed_radiant'),0)>0
                        OR COALESCE(json_extract(payload,'$.bet_seed_dire'),0)>0
                        OR COALESCE(json_extract(payload,'$.bet_seed_bonus'),0)>0))",
            params![guild_id,pending_match_id], |row| row.get(0),
        )?;
        if finances_pending {
            return Err(PendingMatchRepositoryError::FinancialAbortRequired(
                pending_match_id,
            ));
        }
        let changed = transaction.execute(
            "DELETE FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
        )?;
        if changed == 1 {
            for discord_id in participant_ids.iter().copied().collect::<BTreeSet<_>>() {
                transaction.execute(
                    "UPDATE players
                     SET exclusion_count = COALESCE(exclusion_count, 0) + 1,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, guild_id],
                )?;
            }
        }
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Delete every pending row for a guild. The count is returned so callers
    /// can distinguish a no-op from a legacy bulk clear.
    pub fn delete_pending_matches(
        &self,
        guild_id: i64,
    ) -> Result<usize, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .execute(
                "DELETE FROM pending_matches WHERE guild_id=?1",
                params![guild_id],
            )
            .map_err(Into::into)
    }

    /// Atomically return and delete one pending match.
    ///
    /// With an explicit ID this consumes only that guild-scoped row. Without
    /// an ID it preserves the legacy Python contract and consumes only when
    /// the guild has exactly one pending match.
    pub fn consume_pending_match(
        &self,
        guild_id: i64,
        pending_match_id: Option<i64>,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut rows = if let Some(pending_match_id) = pending_match_id {
            query_raw(&transaction, guild_id, pending_match_id)?
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            query_all_raw(&transaction, guild_id)?
        };
        if rows.len() != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let pending = decode_raw(rows.pop().expect("one pending row"))?;
        let changed = transaction.execute(
            "DELETE FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending.pending_match_id],
        )?;
        if changed != 1 {
            return Err(PendingMatchRepositoryError::PendingMatchNotFound(
                pending.pending_match_id,
            ));
        }
        transaction.commit()?;
        Ok(Some(pending))
    }

    /// Extend betting from `max(current_lock, now)` under one immediate
    /// SQLite writer lock.
    ///
    /// This mutation operates on the raw JSON object and preserves fields
    /// unknown to this binary. A missing, null, or zero lock is treated as no
    /// betting window, matching the Python command.
    ///
    /// The resulting deadline is also persisted in
    /// [`DOTA_BETTING_EXTENDED_UNTIL`]. Persisting it for every successful
    /// extension preserves an administrator's intent if a queued match is
    /// adopted by Dota automation before gameplay begins. The automatic
    /// closed marker remains in place; [`PendingMatchState::betting_open`] and
    /// the betting service use this explicit deadline to decide whether the
    /// administrator reopened betting.
    pub fn extend_betting_atomic(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        now_unix: i64,
        extension_seconds: i64,
    ) -> Result<PendingBettingExtension, PendingMatchRepositoryError> {
        if extension_seconds <= 0 {
            return Err(PendingMatchRepositoryError::InvalidBettingExtension);
        }

        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recorded = transaction
            .query_row(
                "SELECT match_id FROM matches
                 WHERE guild_id=?1 AND pending_match_id=?2
                 LIMIT 1",
                params![guild_id, pending_match_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if recorded.is_some() {
            return Err(PendingMatchRepositoryError::MatchAlreadyRecorded(
                pending_match_id,
            ));
        }
        let raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let mut payload = decode_value(pending_match_id, &raw.payload)?;
        let object = payload.as_object_mut().ok_or_else(|| {
            PendingMatchRepositoryError::MalformedPayload {
                pending_match_id,
                message: "root value is not an object".to_owned(),
            }
        })?;
        if object.get("draft_setup_complete").and_then(Value::as_bool) == Some(false) {
            return Err(PendingMatchRepositoryError::SetupIncomplete(
                pending_match_id,
            ));
        }
        let old_lock_until = object
            .get("bet_lock_until")
            .and_then(Value::as_i64)
            .filter(|lock| *lock != 0)
            .ok_or(PendingMatchRepositoryError::MissingBettingWindow(
                pending_match_id,
            ))?;
        let new_lock_until = old_lock_until
            .max(now_unix)
            .checked_add(extension_seconds)
            .ok_or(PendingMatchRepositoryError::ArithmeticOverflow)?;
        object.insert("bet_lock_until".to_owned(), Value::from(new_lock_until));
        if new_lock_until > 0 {
            object.insert(
                DOTA_BETTING_EXTENDED_UNTIL.to_owned(),
                Value::from(new_lock_until),
            );
        }
        let encoded = serde_json::to_string(&payload)
            .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE pending_matches
                SET payload=?1,updated_at=CURRENT_TIMESTAMP
              WHERE guild_id=?2 AND pending_match_id=?3",
            params![encoded, guild_id, pending_match_id],
        )?;
        if changed != 1 {
            return Err(PendingMatchRepositoryError::PendingMatchNotFound(
                pending_match_id,
            ));
        }
        let updated_raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let pending_match = decode_raw(updated_raw)?;
        transaction.commit()?;
        Ok(PendingBettingExtension {
            pending_match,
            old_lock_until,
            new_lock_until,
        })
    }

    /// Atomically mark a pending match as owned by Dota lobby automation.
    ///
    /// Adoption leaves `bet_lock_until` unchanged so the original timed
    /// deadline remains available to UI and can be restored simply by
    /// releasing the marker. A separate fresh observation is required before
    /// this marker allows wagers. This operation is idempotent for
    /// an already-adopted row and never reopens a row whose closed marker was
    /// already committed.
    pub fn begin_hosted_betting(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        now_unix: i64,
    ) -> Result<PendingMatchRecord, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recorded = transaction
            .query_row(
                "SELECT match_id FROM matches
                 WHERE guild_id=?1 AND pending_match_id=?2
                 LIMIT 1",
                params![guild_id, pending_match_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if recorded.is_some() {
            return Err(PendingMatchRepositoryError::MatchAlreadyRecorded(
                pending_match_id,
            ));
        }

        let raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let mut payload = decode_value(pending_match_id, &raw.payload)?;
        let object = payload.as_object_mut().ok_or_else(|| {
            PendingMatchRepositoryError::MalformedPayload {
                pending_match_id,
                message: "root value is not an object".to_owned(),
            }
        })?;

        // A discovery pass may have read this match before an administrator
        // switched it to manual hosting. Recheck under the same writer lock
        // as adoption so that stale discovery cannot restore bot ownership.
        let pending_match = decode_raw(raw)?;
        if pending_match
            .state
            .extra
            .get("shuffle_setup_complete")
            .and_then(Value::as_bool)
            == Some(false)
        {
            return Err(PendingMatchRepositoryError::SetupIncomplete(
                pending_match_id,
            ));
        }
        if pending_match
            .state
            .extra
            .get("draft_setup_complete")
            .and_then(Value::as_bool)
            == Some(false)
        {
            return Err(PendingMatchRepositoryError::SetupIncomplete(
                pending_match_id,
            ));
        }
        let hosting_options =
            DotaHostingOptions::from_extra(&pending_match.state.extra).map_err(|message| {
                PendingMatchRepositoryError::MalformedPayload {
                    pending_match_id,
                    message,
                }
            })?;
        if hosting_options.hosting == Some(HostingMode::Manual) {
            transaction.commit()?;
            return Ok(pending_match);
        }

        // A closed row is terminal. Return the current document so callers
        // can inspect the durable state without accidentally reopening it.
        if object
            .get(DOTA_BETTING_CLOSED_MARKER)
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            transaction.commit()?;
            return Ok(pending_match);
        }

        let already_managed = object
            .get(DOTA_HOSTED_BETTING_MARKER)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !already_managed {
            object.insert(DOTA_HOSTED_BETTING_MARKER.to_owned(), Value::Bool(true));
            object.insert(
                DOTA_HOSTED_BETTING_STARTED_AT.to_owned(),
                Value::from(now_unix),
            );
            // A row may be adopted again after a pregame cancellation. The
            // old release timestamp is only audit data and must not make a
            // new adoption look cancelled.
            object.remove(DOTA_HOSTED_BETTING_RELEASED_AT);
            object.remove(DOTA_HOSTED_BETTING_OBSERVED_AT);
        }
        let encoded = serde_json::to_string(&payload)
            .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE pending_matches
                SET payload=?1,updated_at=CURRENT_TIMESTAMP
              WHERE guild_id=?2 AND pending_match_id=?3",
            params![encoded, guild_id, pending_match_id],
        )?;
        if changed != 1 {
            return Err(PendingMatchRepositoryError::PendingMatchNotFound(
                pending_match_id,
            ));
        }
        let updated_raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let pending_match = decode_raw(updated_raw)?;
        transaction.commit()?;
        Ok(pending_match)
    }

    /// Renew admission only after the host has verified a current owned
    /// lobby snapshot. Adoption and reconnect attempts do not renew this lease.
    pub fn observe_hosted_betting(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        now_unix: i64,
    ) -> Result<bool, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE pending_matches SET payload=json_set(payload,
                '$.dota_hosted_betting_observed_at',?1),updated_at=CURRENT_TIMESTAMP
             WHERE guild_id=?2 AND pending_match_id=?3
               AND json_extract(payload,'$.dota_hosted_betting')=1
               AND COALESCE(json_extract(payload,'$.dota_betting_closed'),0)=0
               AND COALESCE(json_extract(payload,'$.shuffle_setup_complete'),1)=1
               AND COALESCE(json_extract(payload,'$.draft_setup_complete'),1)=1
               AND NOT EXISTS(SELECT 1 FROM matches WHERE guild_id=?2 AND pending_match_id=?3)",
            params![now_unix, guild_id, pending_match_id],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Revoke a previous observation immediately on connection/ownership loss.
    /// An explicit administrator extension remains a separate policy decision.
    pub fn suspend_hosted_betting(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<bool, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection.execute(
            "UPDATE pending_matches SET payload=json_remove(payload,
                '$.dota_hosted_betting_observed_at'),updated_at=CURRENT_TIMESTAMP
             WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
        )? == 1)
    }

    /// Explicit operator stop, stronger than a bounded betting extension.
    /// Resuming clears old observations so automatic admission needs new evidence.
    pub fn set_betting_suspended(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        suspended: bool,
    ) -> Result<bool, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection.execute(
            "UPDATE pending_matches SET payload=json_set(json_remove(payload,
                '$.dota_hosted_betting_observed_at'),'$.dota_betting_suspended',json(?1)),
                updated_at=CURRENT_TIMESTAMP
             WHERE guild_id=?2 AND pending_match_id=?3
               AND NOT EXISTS(SELECT 1 FROM matches WHERE guild_id=?2 AND pending_match_id=?3)",
            params![
                if suspended { "true" } else { "false" },
                guild_id,
                pending_match_id
            ],
        )? == 1)
    }

    /// Apply and journal an operator betting control in one writer transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn set_betting_suspended_audited(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        suspended: bool,
        actor_id: i64,
        reason: &str,
        now_unix: i64,
    ) -> Result<bool, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = query_raw(&transaction, guild_id, pending_match_id)?;
        let Some(raw) = raw else {
            return Ok(false);
        };
        if transaction
            .query_row(
                "SELECT 1 FROM matches WHERE guild_id=?1 AND pending_match_id=?2 LIMIT 1",
                params![guild_id, pending_match_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(PendingMatchRepositoryError::MatchAlreadyRecorded(
                pending_match_id,
            ));
        }
        let mut pending = decode_raw(raw)?;
        if pending
            .state
            .extra
            .get("draft_setup_complete")
            .and_then(Value::as_bool)
            == Some(false)
        {
            return Err(PendingMatchRepositoryError::SetupIncomplete(
                pending_match_id,
            ));
        }
        pending
            .state
            .extra
            .insert("dota_betting_suspended".into(), Value::Bool(suspended));
        pending.state.extra.remove(DOTA_HOSTED_BETTING_OBSERVED_AT);
        let audit = pending
            .state
            .extra
            .entry("dota_betting_control_audit".to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        let history =
            audit
                .as_array_mut()
                .ok_or_else(|| PendingMatchRepositoryError::MalformedPayload {
                    pending_match_id,
                    message: "betting control audit must be an array".to_owned(),
                })?;
        history.push(
            serde_json::json!({"action":if suspended {"suspend"} else {"resume"},
            "actor_id":actor_id,"reason":reason,"time":now_unix}),
        );
        transaction.execute("UPDATE pending_matches SET payload=?1,updated_at=CURRENT_TIMESTAMP WHERE guild_id=?2 AND pending_match_id=?3",
            params![encode_state(&pending.state)?,guild_id,pending_match_id])?;
        let session_revision = transaction
            .query_row(
                "SELECT revision FROM dota_sessions WHERE guild_id=?1 AND pending_match_id=?2",
                params![guild_id, pending_match_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(revision) = session_revision {
            let next_revision = revision
                .checked_add(1)
                .ok_or(PendingMatchRepositoryError::ArithmeticOverflow)?;
            let audit = serde_json::json!({"action":if suspended {"suspend"} else {"resume"},
                "actor_id":actor_id,"reason":reason,"time":now_unix});
            transaction.execute(
                "UPDATE dota_sessions SET payload=json_insert(
                    CASE WHEN json_type(payload,'$.betting_control_audit')='array' THEN payload
                         ELSE json_set(payload,'$.betting_control_audit',json('[]')) END,
                    '$.betting_control_audit[#]',json(?1)),revision=?2,updated_at=?3
                 WHERE guild_id=?4 AND pending_match_id=?5",
                params![
                    audit.to_string(),
                    next_revision,
                    now_unix,
                    guild_id,
                    pending_match_id
                ],
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    /// Atomically release a hosted-betting adoption before the game starts.
    ///
    /// Releasing only removes the adoption marker and leaves the original
    /// `bet_lock_until` in place, restoring normal timed betting semantics.
    /// Once [`Self::close_betting_now`] has committed the closed marker this
    /// operation refuses to reopen the window, even if a stale cancellation
    /// arrives later.
    pub fn release_hosted_betting(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        now_unix: i64,
    ) -> Result<PendingMatchRecord, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recorded = transaction
            .query_row(
                "SELECT match_id FROM matches
                 WHERE guild_id=?1 AND pending_match_id=?2
                 LIMIT 1",
                params![guild_id, pending_match_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if recorded.is_some() {
            return Err(PendingMatchRepositoryError::MatchAlreadyRecorded(
                pending_match_id,
            ));
        }

        let raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let mut payload = decode_value(pending_match_id, &raw.payload)?;
        let object = payload.as_object_mut().ok_or_else(|| {
            PendingMatchRepositoryError::MalformedPayload {
                pending_match_id,
                message: "root value is not an object".to_owned(),
            }
        })?;

        if object
            .get(DOTA_BETTING_CLOSED_MARKER)
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(PendingMatchRepositoryError::BettingClosedByDotaSession(
                pending_match_id,
            ));
        }

        if release_hosted_betting_payload(object, now_unix) {
            let encoded = serde_json::to_string(&payload)
                .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))?;
            let changed = transaction.execute(
                "UPDATE pending_matches
                    SET payload=?1,updated_at=CURRENT_TIMESTAMP
                  WHERE guild_id=?2 AND pending_match_id=?3",
                params![encoded, guild_id, pending_match_id],
            )?;
            if changed != 1 {
                return Err(PendingMatchRepositoryError::PendingMatchNotFound(
                    pending_match_id,
                ));
            }
        }
        let updated_raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let pending_match = decode_raw(updated_raw)?;
        transaction.commit()?;
        Ok(pending_match)
    }

    /// Atomically close one pending match's betting window and mark its JSON
    /// document as Dota-owned.
    ///
    /// The immediate transaction serializes this operation with betting,
    /// extension, voting, recording, and pending-row cleanup.  A completed
    /// match is rejected so a late lobby worker cannot mutate an identity
    /// that has already entered the recording path.  Repeating the close is
    /// idempotent: the lock is shortened for an ordinary timed window, while
    /// an explicit Dota extension deadline is preserved across repeated
    /// closes. The durable closed marker remains true in either case.
    pub fn close_betting_now(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        now_unix: i64,
    ) -> Result<PendingBettingClose, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recorded = transaction
            .query_row(
                "SELECT match_id FROM matches
                 WHERE guild_id=?1 AND pending_match_id=?2
                 LIMIT 1",
                params![guild_id, pending_match_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if recorded.is_some() {
            return Err(PendingMatchRepositoryError::MatchAlreadyRecorded(
                pending_match_id,
            ));
        }

        let raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let mut payload = decode_value(pending_match_id, &raw.payload)?;
        let object = payload.as_object_mut().ok_or_else(|| {
            PendingMatchRepositoryError::MalformedPayload {
                pending_match_id,
                message: "root value is not an object".to_owned(),
            }
        })?;
        let old_lock_until = object
            .get("bet_lock_until")
            .and_then(Value::as_i64)
            .filter(|lock| *lock != 0);
        let new_lock_until = object
            .get(DOTA_BETTING_EXTENDED_UNTIL)
            .and_then(Value::as_i64)
            .filter(|extension_until| *extension_until > 0)
            .or_else(|| old_lock_until.map(|lock| lock.min(now_unix)))
            .unwrap_or(now_unix);
        object.insert("bet_lock_until".to_owned(), Value::from(new_lock_until));
        object.insert(DOTA_BETTING_CLOSED_MARKER.to_owned(), Value::Bool(true));
        let encoded = serde_json::to_string(&payload)
            .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE pending_matches
                SET payload=?1,updated_at=CURRENT_TIMESTAMP
              WHERE guild_id=?2 AND pending_match_id=?3",
            params![encoded, guild_id, pending_match_id],
        )?;
        if changed != 1 {
            return Err(PendingMatchRepositoryError::PendingMatchNotFound(
                pending_match_id,
            ));
        }
        let updated_raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let pending_match = decode_raw(updated_raw)?;
        transaction.commit()?;
        Ok(PendingBettingClose {
            pending_match,
            old_lock_until,
            new_lock_until,
        })
    }
}

/// Release only adoption metadata; timed deadlines and terminal close markers
/// retain their existing authority. Repeated cleanup is a no-op.
fn release_hosted_betting_payload(
    object: &mut serde_json::Map<String, Value>,
    now_unix: i64,
) -> bool {
    let reserved = object.remove("dota_host_account_key").is_some();
    if reserved {
        let hosting = object
            .entry("dota_hosting".to_owned())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(hosting) = hosting.as_object_mut() {
            hosting.insert("hosting".into(), Value::String("manual".into()));
        }
        object
            .entry("dota_hosting_fallback_reason".to_owned())
            .or_insert_with(|| Value::String("hosting_cancelled".into()));
    }
    object.remove("dota_hosted_betting_observed_at");
    let managed = object
        .get(DOTA_HOSTED_BETTING_MARKER)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if managed {
        object.remove(DOTA_HOSTED_BETTING_MARKER);
        object.remove(DOTA_HOSTED_BETTING_STARTED_AT);
        object.insert(
            DOTA_HOSTED_BETTING_RELEASED_AT.to_owned(),
            Value::from(now_unix),
        );
    }
    managed || reserved
}

#[derive(Debug)]
struct RawPendingMatch {
    pending_match_id: i64,
    guild_id: i64,
    payload: String,
    created_at: Option<String>,
    updated_at: Option<String>,
}

fn raw_from_row(row: &rusqlite::Row<'_>) -> Result<RawPendingMatch, rusqlite::Error> {
    Ok(RawPendingMatch {
        pending_match_id: row.get(0)?,
        guild_id: row.get(1)?,
        payload: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn query_raw(
    connection: &Connection,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<Option<RawPendingMatch>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT pending_match_id,guild_id,payload,created_at,updated_at
               FROM pending_matches
              WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
            raw_from_row,
        )
        .optional()
}

fn query_all_raw(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<RawPendingMatch>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT pending_match_id,guild_id,payload,created_at,updated_at
           FROM pending_matches
          WHERE guild_id=?1
          ORDER BY created_at,pending_match_id",
    )?;
    statement
        .query_map(params![guild_id], raw_from_row)?
        .collect()
}

fn decode_value(
    pending_match_id: i64,
    payload: &str,
) -> Result<Value, PendingMatchRepositoryError> {
    serde_json::from_str(payload).map_err(|error| PendingMatchRepositoryError::MalformedPayload {
        pending_match_id,
        message: error.to_string(),
    })
}

fn decode_raw(raw: RawPendingMatch) -> Result<PendingMatchRecord, PendingMatchRepositoryError> {
    let state = serde_json::from_str::<PendingMatchState>(&raw.payload).map_err(|error| {
        PendingMatchRepositoryError::MalformedPayload {
            pending_match_id: raw.pending_match_id,
            message: error.to_string(),
        }
    })?;
    Ok(PendingMatchRecord {
        pending_match_id: raw.pending_match_id,
        guild_id: raw.guild_id,
        state,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}

fn encode_state(state: &PendingMatchState) -> Result<String, PendingMatchRepositoryError> {
    let mut payload = serde_json::to_value(state)
        .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))?;
    if let Some(object) = payload.as_object_mut() {
        // This is row metadata even if a caller manually placed it in `extra`.
        object.remove("pending_match_id");
    }
    serde_json::to_string(&payload)
        .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    const GUILD_A: i64 = 7;
    const GUILD_B: i64 = 8;

    fn dota_manual_fixture() -> (NamedTempFile, PendingMatchRepository, PendingMatchRecord) {
        let file = NamedTempFile::new().unwrap();
        crate::schema_manager::initialize_or_migrate(file.path()).unwrap();
        let repository = PendingMatchRepository::new(file.path());
        let pending = repository
            .create_pending_match(
                GUILD_A,
                &PendingMatchState {
                    extra: BTreeMap::from([
                        (
                            "dota_hosting".into(),
                            json!({"region":31,"game_mode":2,"tv_delay":0}),
                        ),
                        ("future_metadata".into(), json!({"keep":true})),
                    ]),
                    ..Default::default()
                },
            )
            .unwrap();
        (file, repository, pending)
    }

    fn insert_dota_session(
        file: &NamedTempFile,
        pending: &PendingMatchRecord,
        phase: &str,
        valve_match_id: Option<&str>,
        payload: Value,
    ) {
        Connection::open(file.path()).unwrap().execute(
            "INSERT INTO dota_sessions(guild_id,pending_match_id,account_key,phase,valve_match_id,payload,revision,created_at,updated_at)
             VALUES(?1,?2,'test-host',?3,?4,?5,4,1,1)",
            params![pending.guild_id, pending.pending_match_id, phase, valve_match_id, payload.to_string()],
        ).unwrap();
    }

    #[test]
    fn managed_shuffle_creation_serializes_participant_exclusivity_and_recovery_queue() {
        use std::sync::{Arc, Barrier};
        let (file, repository) = fixture();
        Connection::open(file.path()).unwrap().execute_batch(
            "CREATE TABLE IF NOT EXISTS matches(match_id INTEGER PRIMARY KEY,guild_id INTEGER,pending_match_id INTEGER)"
        ).unwrap();
        let mut pending = state(&[11, 12]);
        pending
            .extra
            .insert("shuffle_setup_complete".into(), json!(false));
        let barrier = Arc::new(Barrier::new(2));
        let other_barrier = barrier.clone();
        let other_state = pending.clone();
        let other_path = file.path().to_path_buf();
        let other = std::thread::spawn(move || {
            other_barrier.wait();
            PendingMatchRepository::new(other_path).create_pending_match(GUILD_A, &other_state)
        });
        barrier.wait();
        let first = repository.create_pending_match(GUILD_A, &pending);
        let second = other.join().unwrap();
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let error = first.as_ref().err().or(second.as_ref().err()).unwrap();
        assert!(matches!(
            error,
            PendingMatchRepositoryError::ParticipantAlreadyPending(_)
        ));
        let ready = first.ok().or(second.ok()).unwrap();
        assert_eq!(repository.unfinished_shuffle_setups().unwrap().len(), 1);
        repository
            .mutate_pending_match(GUILD_A, ready.pending_match_id, |state| {
                state
                    .extra
                    .insert("shuffle_setup_complete".into(), json!(true));
            })
            .unwrap();
        assert!(repository.unfinished_shuffle_setups().unwrap().is_empty());
        // The same Discord identities remain independent across guilds.
        assert!(repository.create_pending_match(GUILD_B, &pending).is_ok());
    }

    #[test]
    fn manual_dota_override_atomically_marks_cleanup_and_preserves_match_metadata() {
        let (file, repository, pending) = dota_manual_fixture();
        insert_dota_session(
            &file,
            &pending,
            "gathering",
            None,
            json!({"launch_requested_at":null,"cancel_requested":false,"future_state":17}),
        );
        assert!(matches!(
            repository.request_manual_dota_hosting(GUILD_B, pending.pending_match_id),
            Err(PendingMatchRepositoryError::PendingMatchNotFound(_))
        ));
        let updated = repository
            .request_manual_dota_hosting(GUILD_A, pending.pending_match_id)
            .unwrap();
        let options = DotaHostingOptions::from_extra(&updated.state.extra).unwrap();
        assert_eq!(options.hosting, Some(HostingMode::Manual));
        assert_eq!(options.region, Some(31));
        assert_eq!(options.game_mode, Some(2));
        assert_eq!(options.tv_delay, Some(0));
        assert_eq!(updated.state.extra["future_metadata"], json!({"keep":true}));
        let connection = Connection::open(file.path()).unwrap();
        let (payload, revision, phase): (String, i64, String) = connection.query_row(
            "SELECT payload,revision,phase FROM dota_sessions WHERE guild_id=?1 AND pending_match_id=?2",
            params![GUILD_A, pending.pending_match_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&payload).unwrap(),
            json!({"launch_requested_at":null,"cancel_requested":true,"future_state":17})
        );
        assert_eq!(revision, 5);
        assert_eq!(
            phase, "gathering",
            "worker must confirm external cleanup before releasing lease"
        );
    }

    #[test]
    fn manual_dota_override_rejects_every_started_evidence_without_mutating_either_row() {
        for (phase, valve_match_id, payload) in [
            ("launching", None, json!({})),
            ("running", None, json!({})),
            ("finishing", None, json!({})),
            ("needs_review", Some("123456"), json!({})),
            ("gathering", None, json!({"launch_requested_at":42})),
        ] {
            let (file, repository, pending) = dota_manual_fixture();
            insert_dota_session(&file, &pending, phase, valve_match_id, payload.clone());
            assert!(
                matches!(
                    repository.request_manual_dota_hosting(GUILD_A, pending.pending_match_id),
                    Err(PendingMatchRepositoryError::DotaSessionAlreadyStarted(_))
                ),
                "phase {phase}"
            );
            assert_eq!(
                repository
                    .pending_match(GUILD_A, pending.pending_match_id)
                    .unwrap()
                    .unwrap(),
                pending
            );
            let (stored, revision): (String, i64) = Connection::open(file.path())
                .unwrap()
                .query_row("SELECT payload,revision FROM dota_sessions", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .unwrap();
            assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), payload);
            assert_eq!(revision, 4);
        }
    }

    #[test]
    fn manual_dota_override_works_before_discovery_and_preserves_terminal_history() {
        for terminal in [None, Some("cancelled"), Some("failed"), Some("recorded")] {
            let (file, repository, pending) = dota_manual_fixture();
            if let Some(phase) = terminal {
                insert_dota_session(
                    &file,
                    &pending,
                    phase,
                    Some("123456"),
                    json!({"launch_requested_at":42}),
                );
            }
            let updated = repository
                .request_manual_dota_hosting(GUILD_A, pending.pending_match_id)
                .unwrap();
            assert_eq!(
                DotaHostingOptions::from_extra(&updated.state.extra)
                    .unwrap()
                    .hosting,
                Some(HostingMode::Manual)
            );
            if terminal.is_some() {
                let revision: i64 = Connection::open(file.path())
                    .unwrap()
                    .query_row("SELECT revision FROM dota_sessions", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(revision, 4);
            }
        }
    }

    #[test]
    fn manual_dota_override_fails_closed_for_malformed_session_payload() {
        let (file, repository, pending) = dota_manual_fixture();
        insert_dota_session(&file, &pending, "needs_review", None, json!([]));
        assert!(matches!(
            repository.request_manual_dota_hosting(GUILD_A, pending.pending_match_id),
            Err(PendingMatchRepositoryError::MalformedPayload { .. })
        ));
        assert_eq!(
            repository
                .pending_match(GUILD_A, pending.pending_match_id)
                .unwrap()
                .unwrap(),
            pending
        );
    }

    #[test]
    fn manual_dota_override_releases_queued_betting_and_stale_adoption_cannot_reopen_it() {
        for has_session in [false, true] {
            let (file, repository, pending) = dota_manual_fixture();
            repository
                .mutate_pending_match(GUILD_A, pending.pending_match_id, |state| {
                    state.bet_lock_until = Some(1_000);
                })
                .unwrap();
            let adopted = repository
                .begin_hosted_betting(GUILD_A, pending.pending_match_id, 2_000)
                .unwrap();
            assert!(adopted.state.hosted_betting_managed());
            assert!(!adopted.state.betting_open(2_001));
            assert!(
                repository
                    .observe_hosted_betting(GUILD_A, pending.pending_match_id, 2_000)
                    .unwrap()
            );
            let observed = repository
                .pending_match(GUILD_A, pending.pending_match_id)
                .unwrap()
                .unwrap();
            assert!(observed.state.betting_open(2_001));
            assert!(!observed.state.betting_open(2_090));
            if has_session {
                insert_dota_session(&file, &pending, "gathering", None, json!({}));
            }

            let manual = repository
                .request_manual_dota_hosting(GUILD_A, pending.pending_match_id)
                .unwrap();
            assert!(!manual.state.hosted_betting_managed());
            assert_eq!(manual.state.bet_lock_until, Some(1_000));
            assert!(manual.state.betting_open(999));
            assert!(!manual.state.betting_open(1_000));
            assert!(!manual.state.betting_open(2_001));
            assert!(
                !manual
                    .state
                    .extra
                    .contains_key(DOTA_HOSTED_BETTING_STARTED_AT)
            );
            assert!(
                manual
                    .state
                    .extra
                    .contains_key(DOTA_HOSTED_BETTING_RELEASED_AT)
            );
            assert_eq!(manual.state.extra["future_metadata"], json!({"keep":true}));

            let stale = PendingMatchRepository::new(file.path())
                .begin_hosted_betting(GUILD_A, pending.pending_match_id, 3_000)
                .unwrap();
            assert_eq!(
                stale.state, manual.state,
                "stale discovery must leave manual timed betting intact"
            );
            let cleanup = repository
                .release_hosted_betting(GUILD_A, pending.pending_match_id, 4_000)
                .unwrap();
            assert_eq!(
                cleanup.state, manual.state,
                "later worker cleanup remains idempotent"
            );
        }
    }

    fn fixture() -> (NamedTempFile, PendingMatchRepository) {
        let file = NamedTempFile::new().expect("temporary database");
        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute_batch(
                "CREATE TABLE pending_matches (
                    pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER NOT NULL,
                    payload TEXT NOT NULL,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                );
                 CREATE TABLE IF NOT EXISTS matches(match_id INTEGER PRIMARY KEY,guild_id INTEGER,pending_match_id INTEGER);
                 CREATE TABLE app_kv(guild_id INTEGER,key TEXT,value TEXT,PRIMARY KEY(guild_id,key));
                 CREATE INDEX idx_pending_matches_guild
                    ON pending_matches(guild_id);",
            )
            .expect("create fixture schema");
        drop(connection);
        let repository = PendingMatchRepository::new(file.path());
        (file, repository)
    }

    fn create_abort_fixture_schema(connection: &Connection) {
        connection
            .execute_batch(
                "CREATE TABLE players (
                    discord_id INTEGER NOT NULL,
                    guild_id INTEGER NOT NULL,
                    exclusion_count INTEGER NOT NULL DEFAULT 0,
                    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (discord_id, guild_id)
                );
                 CREATE TABLE IF NOT EXISTS matches (
                    match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER,
                    pending_match_id INTEGER
                );
                CREATE TABLE bets(guild_id INTEGER,pending_match_id INTEGER,match_id INTEGER);
                CREATE TABLE first_game_pool_claims(guild_id INTEGER,pending_match_id INTEGER,settled INTEGER);",
            )
            .expect("create abort fixture schema");
    }

    fn state(players: &[i64]) -> PendingMatchState {
        PendingMatchState {
            radiant_team_ids: players.iter().copied().take(2).collect(),
            dire_team_ids: players.iter().copied().skip(2).collect(),
            bet_lock_until: Some(1_000),
            ..PendingMatchState::default()
        }
    }

    #[test]
    fn recorded_cleanup_atomically_archives_policy_and_audit_before_deleting_pending() {
        let (file, repository) = fixture();
        let connection = Connection::open(file.path()).unwrap();
        let mut state = state(&[11, 12]);
        state
            .extra
            .insert("recording_policy".into(), json!({"taxable":[11]}));
        state.extra.insert(
            "dota_betting_control_audit".into(),
            json!([{"action":"suspend","actor_id":1}]),
        );
        let pending = repository.create_pending_match(GUILD_A, &state).unwrap();
        connection
            .execute(
                "INSERT INTO matches(match_id,guild_id,pending_match_id) VALUES (7,?1,?2)",
                params![GUILD_A, pending.pending_match_id],
            )
            .unwrap();
        connection.execute_batch("CREATE TRIGGER reject_archive BEFORE INSERT ON app_kv BEGIN SELECT RAISE(ABORT,'archive unavailable'); END").unwrap();
        assert!(
            repository
                .delete_pending_match(GUILD_A, pending.pending_match_id)
                .is_err()
        );
        assert!(
            repository
                .pending_match(GUILD_A, pending.pending_match_id)
                .unwrap()
                .is_some()
        );
        connection
            .execute_batch("DROP TRIGGER reject_archive")
            .unwrap();
        assert!(
            repository
                .delete_pending_match(GUILD_A, pending.pending_match_id)
                .unwrap()
        );
        assert!(
            !repository
                .delete_pending_match(GUILD_A, pending.pending_match_id)
                .unwrap()
        );
        let saved: String = connection
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id=?1 AND key='match-finalization:7'",
                [GUILD_A],
                |row| row.get(0),
            )
            .unwrap();
        let saved: PendingMatchState = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved.extra, state.extra);
    }

    #[test]
    fn audited_betting_stop_preserves_session_evidence_and_invalidates_stale_worker_revision() {
        let file = NamedTempFile::new().unwrap();
        crate::test_support::copy_migrated_database(file.path()).unwrap();
        let repository = PendingMatchRepository::new(file.path());
        let pending = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .unwrap();
        let sessions = crate::dota_session_repository::DotaSessionRepository::new(file.path());
        sessions
            .claim_session(
                GUILD_A,
                pending.pending_match_id,
                "test-host",
                json!({}),
                100,
            )
            .unwrap();
        assert!(
            repository
                .set_betting_suspended_audited(
                    GUILD_A,
                    pending.pending_match_id,
                    true,
                    9,
                    "observation lost",
                    101
                )
                .unwrap()
        );
        assert!(
            repository
                .set_betting_suspended_audited(
                    GUILD_A,
                    pending.pending_match_id,
                    false,
                    9,
                    "verified lobby",
                    102
                )
                .unwrap()
        );
        let connection = Connection::open(file.path()).unwrap();
        let (payload,revision):(String,i64) = connection.query_row("SELECT payload,revision FROM dota_sessions WHERE guild_id=?1 AND pending_match_id=?2",params![GUILD_A,pending.pending_match_id],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(revision, 2);
        let payload: Value = serde_json::from_str(&payload).unwrap();
        let current = repository
            .pending_match(GUILD_A, pending.pending_match_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            payload["betting_control_audit"],
            current.state.extra["dota_betting_control_audit"]
        );
        assert_eq!(
            payload["betting_control_audit"].as_array().unwrap().len(),
            2
        );
    }

    #[test]
    fn typed_round_trip_preserves_unknown_fields_but_not_row_id() {
        let (_file, repository) = fixture();
        let mut pending = state(&[11, 12, 13]);
        pending.extra.insert(
            "future_runtime_field".to_owned(),
            json!({"nested": [1, 2, 3]}),
        );
        pending
            .extra
            .insert("pending_match_id".to_owned(), json!(999));

        let created = repository
            .create_pending_match(GUILD_A, &pending)
            .expect("create pending match");
        assert_eq!(created.state.betting_mode, "pool");
        assert_eq!(
            created.state.extra.get("future_runtime_field"),
            Some(&json!({"nested": [1, 2, 3]}))
        );
        assert!(!created.state.extra.contains_key("pending_match_id"));

        let raw = Connection::open(repository.path)
            .expect("open fixture")
            .query_row(
                "SELECT payload FROM pending_matches WHERE pending_match_id=?1",
                [created.pending_match_id],
                |row| row.get::<_, String>(0),
            )
            .expect("read raw payload");
        assert!(!raw.contains("pending_match_id"));
    }

    #[test]
    fn single_and_player_selection_are_scoped_and_deterministic() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&[11, 12, 13]))
            .expect("create first");
        let mut second_state = state(&[21, 22, 23]);
        second_state.excluded_player_ids = vec![90];
        let second = repository
            .create_pending_match(GUILD_A, &second_state)
            .expect("create second");
        repository
            .create_pending_match(GUILD_B, &state(&[11, 99]))
            .expect("create other guild");

        assert!(
            repository
                .single_pending_match(GUILD_A)
                .expect("select single")
                .is_none()
        );
        assert_eq!(
            repository
                .pending_match_for_participant(GUILD_A, 11)
                .expect("participant lookup")
                .expect("participant match")
                .pending_match_id,
            first.pending_match_id
        );
        assert!(
            repository
                .pending_match_for_participant(GUILD_A, 90)
                .expect("participant lookup")
                .is_none()
        );
        assert_eq!(
            repository
                .pending_match_for_full_lobby_player(GUILD_A, 90)
                .expect("full lobby lookup")
                .expect("full lobby match")
                .pending_match_id,
            second.pending_match_id
        );
        assert_eq!(
            repository
                .all_pending_player_ids(GUILD_A)
                .expect("all pending ids"),
            BTreeSet::from([11, 12, 13, 21, 22, 23])
        );
    }

    #[test]
    fn update_and_delete_have_strict_guild_guards() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create pending match");
        let mut changed_state = created.state.clone();
        changed_state.lobby_kind = Some("open".to_owned());

        assert!(
            !repository
                .update_pending_match(GUILD_B, created.pending_match_id, &changed_state)
                .expect("cross-guild update")
        );
        assert_eq!(
            repository
                .pending_match(GUILD_A, created.pending_match_id)
                .expect("load pending")
                .expect("pending exists")
                .state
                .lobby_kind,
            None
        );
        assert!(
            !repository
                .delete_pending_match(GUILD_B, created.pending_match_id)
                .expect("cross-guild delete")
        );
        assert!(
            repository
                .delete_pending_match(GUILD_A, created.pending_match_id)
                .expect("delete pending")
        );
    }

    #[test]
    fn concurrent_finalize_abort_credits_once_and_preserves_pending_and_guild_scopes() {
        let (_file, repository) = fixture();
        let connection = Connection::open(&repository.path).expect("open abort fixture");
        create_abort_fixture_schema(&connection);
        for (guild_id, discord_id, exclusion_count) in [
            (GUILD_A, 11, 5),
            (GUILD_A, 12, 5),
            (GUILD_B, 11, 9),
            (GUILD_B, 12, 9),
            (GUILD_A, 21, 7),
            (GUILD_A, 22, 7),
        ] {
            connection
                .execute(
                    "INSERT INTO players (discord_id,guild_id,exclusion_count)
                     VALUES (?1,?2,?3)",
                    params![discord_id, guild_id, exclusion_count],
                )
                .expect("insert abort player fixture");
        }
        let aborted = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create aborted pending match");
        let sibling = repository
            .create_pending_match(GUILD_A, &state(&[21, 22]))
            .expect("create sibling pending match");

        let barrier = Arc::new(Barrier::new(3));
        let workers = (0..2)
            .map(|_| {
                let repository = repository.clone();
                let barrier = Arc::clone(&barrier);
                let pending_match_id = aborted.pending_match_id;
                thread::spawn(move || {
                    barrier.wait();
                    repository
                        .finalize_abort(GUILD_A, pending_match_id, &[11, 12])
                        .expect("finalize concurrent abort")
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let mut outcomes = workers
            .into_iter()
            .map(|worker| worker.join().expect("join abort worker"))
            .collect::<Vec<_>>();
        outcomes.sort_unstable();
        assert_eq!(outcomes, [false, true]);

        let exclusion_count = |guild_id, discord_id| {
            connection
                .query_row(
                    "SELECT exclusion_count FROM players
                     WHERE guild_id=?1 AND discord_id=?2",
                    params![guild_id, discord_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read abort exclusion count")
        };
        assert_eq!(exclusion_count(GUILD_A, 11), 6);
        assert_eq!(exclusion_count(GUILD_A, 12), 6);
        assert_eq!(exclusion_count(GUILD_B, 11), 9);
        assert_eq!(exclusion_count(GUILD_B, 12), 9);
        assert_eq!(exclusion_count(GUILD_A, 21), 7);
        assert_eq!(exclusion_count(GUILD_A, 22), 7);
        assert!(
            repository
                .pending_match(GUILD_A, aborted.pending_match_id)
                .expect("read aborted pending match")
                .is_none()
        );
        assert!(
            repository
                .pending_match(GUILD_A, sibling.pending_match_id)
                .expect("read sibling pending match")
                .is_some()
        );
    }

    #[test]
    fn legacy_abort_refuses_unresolved_wagers_reserves_or_claims() {
        let (file, repository) = fixture();
        let connection = Connection::open(file.path()).unwrap();
        create_abort_fixture_schema(&connection);
        let pending = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .unwrap();
        connection
            .execute(
                "INSERT INTO bets(guild_id,pending_match_id) VALUES (?1,?2)",
                params![GUILD_A, pending.pending_match_id],
            )
            .unwrap();
        assert!(matches!(
            repository.finalize_abort(GUILD_A, pending.pending_match_id, &[]),
            Err(PendingMatchRepositoryError::FinancialAbortRequired(_))
        ));
        connection.execute("DELETE FROM bets", []).unwrap();
        connection
            .execute(
                "INSERT INTO first_game_pool_claims VALUES (?1,?2,0)",
                params![GUILD_A, pending.pending_match_id],
            )
            .unwrap();
        assert!(matches!(
            repository.finalize_abort(GUILD_A, pending.pending_match_id, &[]),
            Err(PendingMatchRepositoryError::FinancialAbortRequired(_))
        ));
        connection
            .execute("DELETE FROM first_game_pool_claims", [])
            .unwrap();
        repository
            .mutate_pending_match(GUILD_A, pending.pending_match_id, |state| {
                state.bet_seed_reserved = 10
            })
            .unwrap();
        assert!(matches!(
            repository.finalize_abort(GUILD_A, pending.pending_match_id, &[]),
            Err(PendingMatchRepositoryError::FinancialAbortRequired(_))
        ));
        assert!(
            repository
                .pending_match(GUILD_A, pending.pending_match_id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn finalize_abort_refuses_recorded_match_and_grants_no_credit() {
        let (_file, repository) = fixture();
        let connection = Connection::open(&repository.path).expect("open recorded-abort fixture");
        create_abort_fixture_schema(&connection);
        for discord_id in [11, 12] {
            connection
                .execute(
                    "INSERT INTO players (discord_id,guild_id,exclusion_count)
                     VALUES (?1,?2,5)",
                    params![discord_id, GUILD_A],
                )
                .expect("insert recorded-abort player fixture");
        }
        let pending = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create recorded pending match");
        // Another guild's committed match with the same pending identity must
        // not block this guild's abort.
        connection
            .execute(
                "INSERT INTO matches (guild_id,pending_match_id) VALUES (?1,?2)",
                params![GUILD_B, pending.pending_match_id],
            )
            .expect("insert cross-guild match row");
        connection
            .execute(
                "INSERT INTO matches (guild_id,pending_match_id) VALUES (?1,?2)",
                params![GUILD_A, pending.pending_match_id],
            )
            .expect("insert committed match row");

        let refused = repository.finalize_abort(GUILD_A, pending.pending_match_id, &[11, 12]);
        assert!(matches!(
            refused,
            Err(PendingMatchRepositoryError::MatchAlreadyRecorded(id))
                if id == pending.pending_match_id
        ));

        // The refusal leaves the pending row to the record path's own cleanup
        // and grants no exclusion credit on top of the record's accounting.
        assert!(
            repository
                .pending_match(GUILD_A, pending.pending_match_id)
                .expect("read refused pending match")
                .is_some()
        );
        let exclusion_count = |discord_id: i64| {
            connection
                .query_row(
                    "SELECT exclusion_count FROM players
                     WHERE guild_id=?1 AND discord_id=?2",
                    params![GUILD_A, discord_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("read refused exclusion count")
        };
        assert_eq!(exclusion_count(11), 5);
        assert_eq!(exclusion_count(12), 5);

        // Once the committed row is gone the abort is allowed again.
        connection
            .execute(
                "DELETE FROM matches WHERE guild_id=?1 AND pending_match_id=?2",
                params![GUILD_A, pending.pending_match_id],
            )
            .expect("remove committed match row");
        assert!(
            repository
                .finalize_abort(GUILD_A, pending.pending_match_id, &[11, 12])
                .expect("finalize unrecorded abort")
        );
        assert_eq!(exclusion_count(11), 6);
        assert_eq!(exclusion_count(12), 6);
    }

    #[test]
    fn test_consume_pending_match_by_id_guild_guard() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_B, &state(&[11, 12]))
            .expect("create guild B pending match");

        assert!(
            !repository
                .delete_pending_match(GUILD_A, created.pending_match_id)
                .expect("reject cross-guild consume")
        );
        assert!(
            repository
                .pending_match(GUILD_B, created.pending_match_id)
                .expect("load guild B pending match")
                .is_some()
        );
    }

    #[test]
    fn test_update_pending_match_guild_guard() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create guild A pending match");
        let mut tampered = created.state.clone();
        tampered.extra.insert("tampered".to_owned(), json!(true));

        assert!(
            !repository
                .update_pending_match(GUILD_B, created.pending_match_id, &tampered)
                .expect("reject cross-guild update")
        );
        let stored = repository
            .pending_match(GUILD_A, created.pending_match_id)
            .expect("load guild A pending match")
            .expect("pending match exists");
        assert!(!stored.state.extra.contains_key("tampered"));
    }

    #[test]
    fn test_update_pending_match_correct_guild_succeeds() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create guild A pending match");
        let mut updated = created.state.clone();
        updated.extra.insert("updated".to_owned(), json!(true));

        assert!(
            repository
                .update_pending_match(GUILD_A, created.pending_match_id, &updated)
                .expect("update matching guild")
        );
        let stored = repository
            .pending_match(GUILD_A, created.pending_match_id)
            .expect("load guild A pending match")
            .expect("pending match exists");
        assert_eq!(stored.state.extra.get("updated"), Some(&json!(true)));
    }

    #[test]
    fn test_get_pending_match_by_id_guild_guard() {
        let (_file, repository) = fixture();
        let mut original = state(&[11, 12]);
        original.extra.insert("original".to_owned(), json!(true));
        let created = repository
            .create_pending_match(GUILD_A, &original)
            .expect("create guild A pending match");

        assert!(
            repository
                .pending_match(GUILD_B, created.pending_match_id)
                .expect("cross-guild lookup")
                .is_none()
        );
        let stored = repository
            .pending_match(GUILD_A, created.pending_match_id)
            .expect("matching-guild lookup")
            .expect("pending match exists");
        assert_eq!(stored.state.extra.get("original"), Some(&json!(true)));
    }

    #[test]
    fn extension_uses_later_base_and_preserves_unknown_json() {
        let (_file, repository) = fixture();
        Connection::open(&repository.path)
            .expect("open extension fixture")
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS matches (
                match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER,
                pending_match_id INTEGER
            );",
            )
            .expect("create extension matches fixture");
        let mut pending = state(&[11, 12]);
        pending
            .extra
            .insert("future_runtime_field".to_owned(), json!({"keep": true}));
        let created = repository
            .create_pending_match(GUILD_A, &pending)
            .expect("create pending");

        let extension = repository
            .extend_betting_atomic(GUILD_A, created.pending_match_id, 2_000, 300)
            .expect("extend closed window");
        assert_eq!(extension.old_lock_until, 1_000);
        assert_eq!(extension.new_lock_until, 2_300);
        assert_eq!(
            extension
                .pending_match
                .state
                .extra
                .get("future_runtime_field"),
            Some(&json!({"keep": true}))
        );

        let extension = repository
            .extend_betting_atomic(GUILD_A, created.pending_match_id, 2_100, 60)
            .expect("extend open window");
        assert_eq!(extension.old_lock_until, 2_300);
        assert_eq!(extension.new_lock_until, 2_360);
    }

    #[test]
    fn extension_before_host_adoption_survives_gameplay_close() {
        let (_file, repository) = fixture();
        Connection::open(&repository.path)
            .expect("open queued extension fixture")
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS matches (
                    match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER,
                    pending_match_id INTEGER
                );",
            )
            .expect("create queued extension matches fixture");
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create queued pending match");

        // The administrator can extend a queued match before the Dota host
        // has adopted it. The explicit deadline must survive that adoption.
        let extension = repository
            .extend_betting_atomic(GUILD_A, created.pending_match_id, 2_000, 300)
            .expect("extend queued betting");
        assert_eq!(extension.new_lock_until, 2_300);
        assert_eq!(
            extension.pending_match.state.betting_extension_until(),
            Some(2_300)
        );

        let adopted = repository
            .begin_hosted_betting(GUILD_A, created.pending_match_id, 2_100)
            .expect("adopt queued match");
        assert!(adopted.state.hosted_betting_managed());
        assert!(!adopted.state.betting_open(9_000));
        assert_eq!(adopted.state.betting_extension_until(), Some(2_300));

        // Gameplay begins before the explicit deadline. Automatic close must
        // keep the deadline so the admin override remains effective.
        let closed = repository
            .close_betting_now(GUILD_A, created.pending_match_id, 2_200)
            .expect("close at gameplay start");
        assert!(closed.pending_match.state.betting_closed());
        assert_eq!(closed.new_lock_until, 2_300);
        assert_eq!(closed.pending_match.state.bet_lock_until, Some(2_300));
        assert!(closed.pending_match.state.betting_open(2_299));
        assert!(!closed.pending_match.state.betting_open(2_300));
    }

    #[test]
    fn hosted_betting_stays_open_past_deadline_and_release_restores_timer_after_restart() {
        let (file, repository) = fixture();
        Connection::open(&repository.path)
            .expect("open hosted betting fixture")
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS matches (
                    match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER,
                    pending_match_id INTEGER
                );",
            )
            .expect("create hosted betting matches table");
        let mut pending = state(&[11, 12]);
        pending.extra.insert("future_field".to_owned(), json!(true));
        let created = repository
            .create_pending_match(GUILD_A, &pending)
            .expect("create pending");

        // The historical deadline has passed, but host adoption opens the
        // window and preserves that deadline for a possible cancellation.
        let adopted = repository
            .begin_hosted_betting(GUILD_A, created.pending_match_id, 2_000)
            .expect("adopt hosted betting");
        assert!(adopted.state.hosted_betting_managed());
        assert!(!adopted.state.betting_open(2_001));
        assert!(
            repository
                .observe_hosted_betting(GUILD_A, created.pending_match_id, 2_000)
                .unwrap()
        );
        let observed = repository
            .pending_match(GUILD_A, created.pending_match_id)
            .unwrap()
            .unwrap();
        assert!(observed.state.betting_open(2_001));
        assert!(!observed.state.betting_open(2_090));
        assert_eq!(adopted.state.bet_lock_until, Some(1_000));
        assert_eq!(
            adopted.state.extra.get(DOTA_HOSTED_BETTING_STARTED_AT),
            Some(&json!(2_000))
        );
        assert_eq!(adopted.state.extra.get("future_field"), Some(&json!(true)));

        // A fresh repository instance sees the marker and keeps the original
        // adoption timestamp on an idempotent retry.
        let restarted = PendingMatchRepository::new(file.path());
        let retried = restarted
            .begin_hosted_betting(GUILD_A, created.pending_match_id, 3_000)
            .expect("retry hosted adoption after restart");
        assert!(retried.state.hosted_betting_managed());
        assert_eq!(
            retried.state.extra.get(DOTA_HOSTED_BETTING_STARTED_AT),
            Some(&json!(2_000))
        );

        // Pregame cancellation removes ownership while retaining the old
        // deadline, so the ordinary timer policy resumes.
        let released = restarted
            .release_hosted_betting(GUILD_A, created.pending_match_id, 4_000)
            .expect("release hosted betting");
        assert!(!released.state.hosted_betting_managed());
        assert!(!released.state.betting_open(4_000));
        assert_eq!(released.state.bet_lock_until, Some(1_000));
        assert!(
            !released
                .state
                .extra
                .contains_key(DOTA_HOSTED_BETTING_MARKER)
        );
        assert!(
            !released
                .state
                .extra
                .contains_key(DOTA_HOSTED_BETTING_STARTED_AT)
        );
        assert_eq!(
            released.state.extra.get(DOTA_HOSTED_BETTING_RELEASED_AT),
            Some(&json!(4_000))
        );
    }

    #[test]
    fn hosted_betting_is_guild_scoped_and_closed_marker_requires_explicit_extension() {
        let (_file, repository) = fixture();
        Connection::open(&repository.path)
            .expect("open hosted betting fixture")
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS matches (
                    match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER,
                    pending_match_id INTEGER
                );",
            )
            .expect("create hosted betting matches table");
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create pending");

        assert!(matches!(
            repository.begin_hosted_betting(GUILD_B, created.pending_match_id, 2_000),
            Err(PendingMatchRepositoryError::PendingMatchNotFound(id))
                if id == created.pending_match_id
        ));
        assert!(matches!(
            repository.release_hosted_betting(GUILD_B, created.pending_match_id, 2_000),
            Err(PendingMatchRepositoryError::PendingMatchNotFound(id))
                if id == created.pending_match_id
        ));

        let adopted = repository
            .begin_hosted_betting(GUILD_A, created.pending_match_id, 2_000)
            .expect("adopt hosted betting");
        assert!(adopted.state.hosted_betting_managed());
        let extension = repository
            .extend_betting_atomic(GUILD_A, created.pending_match_id, 2_000, 60)
            .expect("extend hosted draft");
        assert_eq!(extension.old_lock_until, 1_000);
        assert_eq!(extension.new_lock_until, 2_060);
        assert_eq!(
            extension.pending_match.state.betting_extension_until(),
            Some(2_060)
        );
        assert_eq!(
            repository
                .pending_match(GUILD_A, created.pending_match_id)
                .unwrap()
                .unwrap()
                .state
                .bet_lock_until,
            extension.pending_match.state.bet_lock_until
        );
        let closed = repository
            .close_betting_now(GUILD_A, created.pending_match_id, 2_100)
            .expect("close hosted betting");
        assert!(closed.pending_match.state.betting_closed());
        assert!(!closed.pending_match.state.hosted_betting_managed());
        assert!(!closed.pending_match.state.betting_open(2_100));

        // Begin is idempotent over the durable closed state, and release
        // cannot turn that final decision back into an open window. An admin
        // extension may explicitly reopen it until its new deadline.
        let retried = repository
            .begin_hosted_betting(GUILD_A, created.pending_match_id, 3_000)
            .expect("retry adoption keeps close");
        assert!(retried.state.betting_closed());
        assert!(!retried.state.hosted_betting_managed());
        assert!(matches!(
            repository.release_hosted_betting(GUILD_A, created.pending_match_id, 3_000),
            Err(PendingMatchRepositoryError::BettingClosedByDotaSession(id))
                if id == created.pending_match_id
        ));
        let reopened = repository
            .extend_betting_atomic(GUILD_A, created.pending_match_id, 3_000, 60)
            .expect("extend closed hosted draft");
        assert_eq!(reopened.old_lock_until, 2_060);
        assert_eq!(reopened.new_lock_until, 3_060);
        assert!(reopened.pending_match.state.betting_closed());
        assert!(reopened.pending_match.state.betting_open(3_059));
        assert!(!reopened.pending_match.state.betting_open(3_060));
    }

    #[test]
    fn close_betting_preserves_explicit_extension_and_repeated_close() {
        let (_file, repository) = fixture();
        let connection = Connection::open(&repository.path).expect("open close fixture");
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS matches (
                    match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER,
                    pending_match_id INTEGER
                );",
            )
            .expect("create matches fixture");
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create pending");
        drop(connection);

        let closed = repository
            .close_betting_now(GUILD_A, created.pending_match_id, 1_500)
            .expect("close betting");
        assert_eq!(closed.old_lock_until, Some(1_000));
        assert_eq!(closed.new_lock_until, 1_000);
        assert_eq!(closed.pending_match.state.bet_lock_until, Some(1_000));
        assert_eq!(
            closed.pending_match.state.extra.get("dota_betting_closed"),
            Some(&json!(true))
        );
        let extension = repository
            .extend_betting_atomic(GUILD_A, created.pending_match_id, 1_500, 60)
            .expect("extend automatically closed betting");
        assert_eq!(extension.old_lock_until, 1_000);
        assert_eq!(extension.new_lock_until, 1_560);
        assert_eq!(
            extension.pending_match.state.betting_extension_until(),
            Some(1_560)
        );
        assert!(extension.pending_match.state.betting_open(1_559));
        assert!(!extension.pending_match.state.betting_open(1_560));

        // A retry with a later wall clock value keeps the administrator's
        // explicit deadline; a repeated close cannot cancel the extension.
        let repeated = repository
            .close_betting_now(GUILD_A, created.pending_match_id, 2_000)
            .expect("repeat close betting");
        assert_eq!(repeated.old_lock_until, Some(1_560));
        assert_eq!(repeated.new_lock_until, 1_560);
        assert_eq!(repeated.pending_match.state.bet_lock_until, Some(1_560));
        assert_eq!(
            repeated.pending_match.state.betting_extension_until(),
            Some(1_560)
        );
    }

    #[test]
    fn close_betting_rejects_recorded_and_cross_guild_rows() {
        let (_file, repository) = fixture();
        let connection = Connection::open(&repository.path).expect("open close fixture");
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS matches (
                    match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER,
                    pending_match_id INTEGER
                );",
            )
            .expect("create matches fixture");
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create pending");
        drop(connection);

        assert!(matches!(
            repository.close_betting_now(GUILD_B, created.pending_match_id, 2_000),
            Err(PendingMatchRepositoryError::PendingMatchNotFound(id)) if id == created.pending_match_id
        ));
        Connection::open(&repository.path)
            .expect("open close fixture")
            .execute(
                "INSERT INTO matches(guild_id,pending_match_id) VALUES(?1,?2)",
                params![GUILD_A, created.pending_match_id],
            )
            .expect("record completed match");
        assert!(matches!(
            repository.close_betting_now(GUILD_A, created.pending_match_id, 2_000),
            Err(PendingMatchRepositoryError::MatchAlreadyRecorded(id)) if id == created.pending_match_id
        ));
        let raw: String = Connection::open(&repository.path)
            .expect("open close fixture")
            .query_row(
                "SELECT payload FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
                params![GUILD_A, created.pending_match_id],
                |row| row.get(0),
            )
            .expect("read pending after refused close");
        assert!(!raw.contains("dota_betting_closed"));
    }

    #[test]
    fn extend_betting_rejects_recorded_rows_but_is_guild_scoped() {
        let (_file, repository) = fixture();
        Connection::open(&repository.path)
            .expect("open extension fixture")
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS matches (
                    match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER,
                    pending_match_id INTEGER
                );",
            )
            .expect("create extension matches fixture");
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create pending");

        // A completed row in another guild with the same pending ID cannot
        // block the guild-owned extension.
        Connection::open(&repository.path)
            .expect("open extension fixture")
            .execute(
                "INSERT INTO matches(guild_id,pending_match_id) VALUES(?1,?2)",
                params![GUILD_B, created.pending_match_id],
            )
            .expect("record sibling guild match");
        let extended = repository
            .extend_betting_atomic(GUILD_A, created.pending_match_id, 2_000, 300)
            .expect("extend despite sibling guild record");
        assert_eq!(extended.new_lock_until, 2_300);

        // Once this guild has a completed match row, extension is rejected
        // atomically even when the pending payload is still present.
        Connection::open(&repository.path)
            .expect("open extension fixture")
            .execute(
                "INSERT INTO matches(guild_id,pending_match_id) VALUES(?1,?2)",
                params![GUILD_A, created.pending_match_id],
            )
            .expect("record matching guild match");
        assert!(matches!(
            repository.extend_betting_atomic(GUILD_A, created.pending_match_id, 2_500, 60),
            Err(PendingMatchRepositoryError::MatchAlreadyRecorded(id))
                if id == created.pending_match_id
        ));
        let pending = repository
            .pending_match(GUILD_A, created.pending_match_id)
            .expect("load pending after rejected extension")
            .expect("pending remains for cleanup");
        assert_eq!(pending.state.bet_lock_until, Some(2_300));
    }

    #[test]
    fn close_betting_closes_legacy_payload_without_a_lock() {
        let (_file, repository) = fixture();
        Connection::open(&repository.path)
            .expect("open close fixture")
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS matches (
                    match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER,
                    pending_match_id INTEGER
                );",
            )
            .expect("create matches fixture");
        let legacy = PendingMatchState {
            bet_lock_until: None,
            ..Default::default()
        };
        let created = repository
            .create_pending_match(GUILD_A, &legacy)
            .expect("create legacy pending");
        let closed = repository
            .close_betting_now(GUILD_A, created.pending_match_id, 3_000)
            .expect("close legacy betting");
        assert_eq!(closed.old_lock_until, None);
        assert_eq!(closed.new_lock_until, 3_000);
        assert_eq!(closed.pending_match.state.bet_lock_until, Some(3_000));
        assert_eq!(
            closed.pending_match.state.extra.get("dota_betting_closed"),
            Some(&json!(true))
        );
    }

    #[test]
    fn atomic_mutation_starts_from_latest_complete_document() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create pending");

        // Stand in for voting's independently committed payload CAS.
        let connection = Connection::open(&repository.path).expect("open fixture");
        let mut payload = serde_json::to_value(&created.state).expect("encode payload");
        payload["record_submissions"] = json!({
            "11": {"result": "radiant", "is_admin": false, "future_vote_field": 9}
        });
        payload["future_root_field"] = json!("keep");
        connection
            .execute(
                "UPDATE pending_matches SET payload=?1 WHERE pending_match_id=?2",
                params![payload.to_string(), created.pending_match_id],
            )
            .expect("persist simulated vote");
        drop(connection);

        let (updated, old_channel) = repository
            .mutate_pending_match(GUILD_A, created.pending_match_id, |state| {
                let old_channel = state.shuffle_channel_id;
                state.shuffle_channel_id = Some(99);
                old_channel
            })
            .expect("atomic metadata mutation")
            .expect("pending exists");
        assert_eq!(old_channel, None);
        assert_eq!(updated.state.shuffle_channel_id, Some(99));
        assert_eq!(
            updated.state.extra.get("future_root_field"),
            Some(&json!("keep"))
        );
        let vote = updated
            .state
            .record_submissions
            .get(&11)
            .expect("vote survived");
        assert_eq!(vote.result.as_deref(), Some("radiant"));
        assert_eq!(vote.extra.get("future_vote_field"), Some(&json!(9)));
    }

    #[test]
    fn test_two_shuffles_create_distinct_pending_matches() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&(1_000..1_010).collect::<Vec<_>>()))
            .expect("create first shuffle");
        let second = repository
            .create_pending_match(GUILD_A, &state(&(2_000..2_010).collect::<Vec<_>>()))
            .expect("create second shuffle");
        assert_ne!(first.pending_match_id, second.pending_match_id);
        assert_eq!(repository.pending_matches(GUILD_A).unwrap().len(), 2);
    }

    #[test]
    fn test_pending_matches_retain_source_lobby_kind() {
        let (_file, repository) = fixture();
        let mut open = state(&(3_000..3_010).collect::<Vec<_>>());
        open.lobby_kind = Some("open".to_owned());
        let mut lowskill = state(&(4_000..4_010).collect::<Vec<_>>());
        lowskill.lobby_kind = Some("lowskill".to_owned());
        let open = repository
            .create_pending_match(GUILD_A, &open)
            .expect("create open shuffle");
        let lowskill = repository
            .create_pending_match(GUILD_A, &lowskill)
            .expect("create lowskill shuffle");
        assert_eq!(open.state.lobby_kind.as_deref(), Some("open"));
        assert_eq!(lowskill.state.lobby_kind.as_deref(), Some("lowskill"));
    }

    #[test]
    fn test_get_last_shuffle_with_specific_id_returns_correct_match() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&[1_000, 1_001, 1_002, 1_003]))
            .expect("create first shuffle");
        let second = repository
            .create_pending_match(GUILD_A, &state(&[2_000, 2_001, 2_002, 2_003]))
            .expect("create second shuffle");
        assert!(repository.single_pending_match(GUILD_A).unwrap().is_none());
        let first_loaded = repository
            .pending_match(GUILD_A, first.pending_match_id)
            .unwrap()
            .expect("load first");
        let second_loaded = repository
            .pending_match(GUILD_A, second.pending_match_id)
            .unwrap()
            .expect("load second");
        assert_eq!(
            first_loaded.state.participant_ids(),
            BTreeSet::from([1_000, 1_001, 1_002, 1_003])
        );
        assert_eq!(
            second_loaded.state.participant_ids(),
            BTreeSet::from([2_000, 2_001, 2_002, 2_003])
        );
    }

    #[test]
    fn test_get_all_pending_player_ids_returns_all_players() {
        let (_file, repository) = fixture();
        repository
            .create_pending_match(GUILD_A, &state(&[11, 12, 13]))
            .expect("create first");
        repository
            .create_pending_match(GUILD_A, &state(&[21, 22, 23]))
            .expect("create second");
        assert_eq!(
            repository.all_pending_player_ids(GUILD_A).unwrap(),
            BTreeSet::from([11, 12, 13, 21, 22, 23])
        );
    }

    #[test]
    fn test_get_pending_match_for_player_finds_correct_match() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&[11, 12, 13]))
            .expect("create first");
        let second = repository
            .create_pending_match(GUILD_A, &state(&[21, 22, 23]))
            .expect("create second");
        assert_eq!(
            repository
                .pending_match_for_participant(GUILD_A, 12)
                .unwrap()
                .expect("first participant")
                .pending_match_id,
            first.pending_match_id
        );
        assert_eq!(
            repository
                .pending_match_for_participant(GUILD_A, 23)
                .unwrap()
                .expect("second participant")
                .pending_match_id,
            second.pending_match_id
        );
    }

    #[test]
    fn test_clear_specific_match_preserves_others() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&[11, 12, 13]))
            .expect("create first");
        let second = repository
            .create_pending_match(GUILD_A, &state(&[21, 22, 23]))
            .expect("create second");
        assert!(
            repository
                .delete_pending_match(GUILD_A, first.pending_match_id)
                .unwrap()
        );
        assert!(
            repository
                .pending_match(GUILD_A, first.pending_match_id)
                .unwrap()
                .is_none()
        );
        assert!(
            repository
                .pending_match(GUILD_A, second.pending_match_id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_consume_pending_match_is_single_use_idempotent() {
        let (_file, repository) = fixture();
        let mut payload = state(&[1, 2, 3]);
        payload.extra.insert("test".to_owned(), json!("data"));
        payload.extra.insert("winning_team".to_owned(), json!(2));
        let created = repository
            .create_pending_match(GUILD_A, &payload)
            .expect("save distinctive pending match");
        let consumed = repository
            .consume_pending_match(GUILD_A, None)
            .expect("consume pending match")
            .expect("first consume returns payload");
        assert_eq!(consumed.pending_match_id, created.pending_match_id);
        assert_eq!(consumed.state, payload);
        assert!(
            repository
                .consume_pending_match(GUILD_A, None)
                .expect("second consume")
                .is_none()
        );
        assert!(
            repository
                .consume_pending_match(GUILD_A, None)
                .expect("third consume")
                .is_none()
        );
    }
}

#[cfg(test)]
#[path = "match_runtime/routing_tests.rs"]
mod routing_tests;
