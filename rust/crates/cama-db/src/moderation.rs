//! Existing-schema persistence for lobby moderation.
//!
//! Python remains the sole schema and migration authority. This module opens
//! only an already-migrated database through [`crate::open_runtime_connection`]
//! and uses `BEGIN IMMEDIATE` for suspension/audit transitions. Schema helpers
//! below are read-only contract checks; production Rust performs no DDL.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuspensionScope {
    All,
    Open,
    Lowskill,
}

impl SuspensionScope {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Open => "open",
            Self::Lowskill => "lowskill",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "open" => Some(Self::Open),
            "lowskill" => Some(Self::Lowskill),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuspensionCompletion {
    Time,
    Matches,
    Either,
    Both,
}

impl SuspensionCompletion {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Matches => "matches",
            Self::Either => "either",
            Self::Both => "both",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "time" => Some(Self::Time),
            "matches" => Some(Self::Matches),
            "either" => Some(Self::Either),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModerationSource {
    Admin,
    Vote,
    Creator,
    System,
}

impl ModerationSource {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Vote => "vote",
            Self::Creator => "creator",
            Self::System => "system",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "vote" => Some(Self::Vote),
            "creator" => Some(Self::Creator),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModerationEventType {
    Suspend,
    Replace,
    Lift,
    Complete,
    Kick,
    LowprioAssign,
    LowprioReplace,
    LowprioClear,
    LowprioComplete,
}

impl ModerationEventType {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Suspend => "suspend",
            Self::Replace => "replace",
            Self::Lift => "lift",
            Self::Complete => "complete",
            Self::Kick => "kick",
            Self::LowprioAssign => "lowprio_assign",
            Self::LowprioReplace => "lowprio_replace",
            Self::LowprioClear => "lowprio_clear",
            Self::LowprioComplete => "lowprio_complete",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "suspend" => Some(Self::Suspend),
            "replace" => Some(Self::Replace),
            "lift" => Some(Self::Lift),
            "complete" => Some(Self::Complete),
            "kick" => Some(Self::Kick),
            "lowprio_assign" => Some(Self::LowprioAssign),
            "lowprio_replace" => Some(Self::LowprioReplace),
            "lowprio_clear" => Some(Self::LowprioClear),
            "lowprio_complete" => Some(Self::LowprioComplete),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LobbySuspension {
    pub discord_id: i64,
    pub guild_id: i64,
    pub scope: SuspensionScope,
    pub completion: SuspensionCompletion,
    pub reason: String,
    pub source: ModerationSource,
    pub actor_id: i64,
    pub expires_at: Option<i64>,
    pub matches_required: Option<i64>,
    pub matches_remaining: Option<i64>,
    pub pending_match_watermark: i64,
    pub revision: i64,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl LobbySuspension {
    #[must_use]
    pub fn matches_completed(&self) -> i64 {
        self.matches_required.map_or(0, |required| {
            required
                .saturating_sub(self.matches_remaining.unwrap_or(0))
                .max(0)
        })
    }

    #[must_use]
    pub fn applies_to(&self, lobby_kind: Option<SuspensionScope>) -> bool {
        lobby_kind.is_none() || self.scope == SuspensionScope::All || lobby_kind == Some(self.scope)
    }

    #[must_use]
    pub fn is_active_at(&self, now: i64) -> bool {
        if !self.active {
            return false;
        }
        let time_pending = self.expires_at.is_some_and(|expires| now < expires);
        let matches_pending = self
            .matches_remaining
            .is_some_and(|remaining| remaining > 0);
        match self.completion {
            SuspensionCompletion::Time => time_pending,
            SuspensionCompletion::Matches => matches_pending,
            SuspensionCompletion::Either => time_pending && matches_pending,
            SuspensionCompletion::Both => time_pending || matches_pending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModerationEvent {
    pub event_id: i64,
    pub guild_id: i64,
    pub discord_id: i64,
    pub event_type: ModerationEventType,
    pub source: ModerationSource,
    pub actor_id: Option<i64>,
    pub reason: Option<String>,
    pub scope: Option<SuspensionScope>,
    pub completion: Option<SuspensionCompletion>,
    pub expires_at: Option<i64>,
    pub matches_required: Option<i64>,
    pub matches_remaining: Option<i64>,
    pub pending_match_watermark: Option<i64>,
    pub wins_required: Option<i64>,
    pub wins_remaining: Option<i64>,
    pub match_id: Option<i64>,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct SaveSuspensionRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub scope: SuspensionScope,
    pub completion: SuspensionCompletion,
    pub expires_at: Option<i64>,
    pub matches_required: Option<i64>,
    pub pending_match_watermark: Option<i64>,
    pub reason: &'a str,
    pub source: ModerationSource,
    pub actor_id: i64,
    pub replace: bool,
    pub now: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct CloseSuspensionRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub event_type: ModerationEventType,
    pub source: ModerationSource,
    pub actor_id: Option<i64>,
    pub reason: Option<&'a str>,
    pub expected_revision: i64,
    pub now: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct RecordModerationEventRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub event_type: ModerationEventType,
    pub source: ModerationSource,
    pub actor_id: Option<i64>,
    pub reason: Option<&'a str>,
    pub scope: Option<SuspensionScope>,
    pub completion: Option<SuspensionCompletion>,
    pub expires_at: Option<i64>,
    pub matches_required: Option<i64>,
    pub matches_remaining: Option<i64>,
    pub pending_match_watermark: Option<i64>,
    pub wins_required: Option<i64>,
    pub wins_remaining: Option<i64>,
    pub match_id: Option<i64>,
    pub now: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct CompletedMatch<'a> {
    pub guild_id: Option<i64>,
    pub pending_match_id: i64,
    pub lobby_kind: &'a str,
    pub match_id: i64,
    pub recorded_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveSuspensionOutcome {
    Saved(LobbySuspension),
    ActiveExists(LobbySuspension),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseSuspensionOutcome {
    Closed(LobbySuspension),
    ConflictOrMissing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressMatchOutcome {
    pub progressed_discord_ids: Vec<i64>,
    pub completed_events: Vec<ModerationEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModerationSchemaContract {
    pub has_lobby_suspensions: bool,
    pub has_moderation_events: bool,
    pub matches_has_lobby_kind: bool,
    pub suspension_columns: BTreeSet<String>,
    pub event_columns: BTreeSet<String>,
    pub low_priority_columns: BTreeSet<String>,
    pub append_only_update_trigger: bool,
    pub append_only_delete_trigger: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LowPriorityMigrationState {
    pub wins_required: i64,
    pub wins_remaining: i64,
    pub start_pending_match_id: Option<i64>,
    pub reason: Option<String>,
}

#[derive(Debug, Error)]
pub enum ModerationRepositoryError {
    #[error("invalid persisted moderation value in {field}: {value}")]
    InvalidPersistedValue { field: &'static str, value: String },
    #[error("moderation SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct ModerationRepository {
    path: PathBuf,
}

impl ModerationRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(guild_id) => guild_id,
            None => 0,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn schema_contract(&self) -> Result<ModerationSchemaContract, ModerationRepositoryError> {
        let connection = self.connection()?;
        Ok(ModerationSchemaContract {
            has_lobby_suspensions: table_exists(&connection, "lobby_suspensions")?,
            has_moderation_events: table_exists(&connection, "moderation_events")?,
            matches_has_lobby_kind: table_columns(&connection, "matches")?.contains("lobby_kind"),
            suspension_columns: table_columns(&connection, "lobby_suspensions")?,
            event_columns: table_columns(&connection, "moderation_events")?,
            low_priority_columns: table_columns(&connection, "low_priority_state")?,
            append_only_update_trigger: trigger_exists(&connection, "moderation_events_no_update")?,
            append_only_delete_trigger: trigger_exists(&connection, "moderation_events_no_delete")?,
        })
    }

    pub fn low_priority_migration_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<LowPriorityMigrationState>, ModerationRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT wins_required,wins_remaining,start_pending_match_id,reason
                 FROM low_priority_state WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(LowPriorityMigrationState {
                        wins_required: row.get(0)?,
                        wins_remaining: row.get(1)?,
                        start_pending_match_id: row.get(2)?,
                        reason: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn pending_match_watermark(
        &self,
        guild_id: Option<i64>,
    ) -> Result<i64, ModerationRepositoryError> {
        let connection = self.connection()?;
        query_watermark(&connection, Self::normalize_guild_id(guild_id)).map_err(Into::into)
    }

    pub fn suspension(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<LobbySuspension>, ModerationRepositoryError> {
        let connection = self.connection()?;
        query_suspension(&connection, discord_id, Self::normalize_guild_id(guild_id))
    }

    pub fn save_suspension(
        &self,
        request: SaveSuspensionRequest<'_>,
    ) -> Result<SaveSuspensionOutcome, ModerationRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = query_suspension(&transaction, request.discord_id, guild_id)?;
        let replacing = existing.as_ref().is_some_and(|state| state.active);
        if replacing && !request.replace {
            transaction.commit()?;
            return Ok(SaveSuspensionOutcome::ActiveExists(
                existing.expect("active state exists"),
            ));
        }
        let watermark = request
            .pending_match_watermark
            .map_or_else(|| query_watermark(&transaction, guild_id), Ok)?;
        transaction.execute(
            "INSERT INTO lobby_suspensions (
                 guild_id,discord_id,scope,completion,expires_at,
                 matches_required,matches_remaining,pending_match_watermark,
                 reason,source,actor_id,active,created_at,updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?6,?7,?8,?9,?10,1,?11,?11)
             ON CONFLICT(guild_id,discord_id) DO UPDATE SET
                 scope=excluded.scope,completion=excluded.completion,
                 expires_at=excluded.expires_at,
                 matches_required=excluded.matches_required,
                 matches_remaining=excluded.matches_remaining,
                 pending_match_watermark=excluded.pending_match_watermark,
                 reason=excluded.reason,source=excluded.source,
                 actor_id=excluded.actor_id,active=1,
                 revision=lobby_suspensions.revision+1,
                 created_at=excluded.created_at,updated_at=excluded.updated_at",
            params![
                guild_id,
                request.discord_id,
                request.scope.name(),
                request.completion.name(),
                request.expires_at,
                request.matches_required,
                watermark,
                request.reason,
                request.source.name(),
                request.actor_id,
                request.now,
            ],
        )?;
        let state = query_suspension(&transaction, request.discord_id, guild_id)?
            .expect("suspension upsert returned no row");
        insert_event(
            &transaction,
            RecordModerationEventRequest {
                discord_id: request.discord_id,
                guild_id: Some(guild_id),
                event_type: if replacing {
                    ModerationEventType::Replace
                } else {
                    ModerationEventType::Suspend
                },
                source: request.source,
                actor_id: Some(request.actor_id),
                reason: Some(request.reason),
                scope: Some(request.scope),
                completion: Some(request.completion),
                expires_at: request.expires_at,
                matches_required: request.matches_required,
                matches_remaining: request.matches_required,
                pending_match_watermark: Some(watermark),
                wins_required: None,
                wins_remaining: None,
                match_id: None,
                now: request.now,
            },
        )?;
        transaction.commit()?;
        Ok(SaveSuspensionOutcome::Saved(state))
    }

    pub fn close_suspension(
        &self,
        request: CloseSuspensionRequest<'_>,
    ) -> Result<CloseSuspensionOutcome, ModerationRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(current) = query_suspension(&transaction, request.discord_id, guild_id)? else {
            transaction.commit()?;
            return Ok(CloseSuspensionOutcome::ConflictOrMissing);
        };
        if !current.active || current.revision != request.expected_revision {
            transaction.commit()?;
            return Ok(CloseSuspensionOutcome::ConflictOrMissing);
        }
        let changed = transaction.execute(
            "UPDATE lobby_suspensions
             SET active=0,revision=revision+1,updated_at=?1
             WHERE discord_id=?2 AND guild_id=?3 AND active=1 AND revision=?4",
            params![
                request.now,
                request.discord_id,
                guild_id,
                request.expected_revision
            ],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(CloseSuspensionOutcome::ConflictOrMissing);
        }
        insert_event(
            &transaction,
            RecordModerationEventRequest {
                discord_id: current.discord_id,
                guild_id: Some(guild_id),
                event_type: request.event_type,
                source: request.source,
                actor_id: request.actor_id,
                reason: request.reason,
                scope: Some(current.scope),
                completion: Some(current.completion),
                expires_at: current.expires_at,
                matches_required: current.matches_required,
                matches_remaining: current.matches_remaining,
                pending_match_watermark: Some(current.pending_match_watermark),
                wins_required: None,
                wins_remaining: None,
                match_id: None,
                now: request.now,
            },
        )?;
        let closed = query_suspension(&transaction, current.discord_id, guild_id)?
            .expect("closed suspension disappeared");
        transaction.commit()?;
        Ok(CloseSuspensionOutcome::Closed(closed))
    }

    pub fn list_suspensions(
        &self,
        guild_id: Option<i64>,
        active_only: bool,
    ) -> Result<Vec<LobbySuspension>, ModerationRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let sql = if active_only {
            format!(
                "{SUSPENSION_SELECT} WHERE s.guild_id=?1 AND s.active=1 ORDER BY s.created_at,s.discord_id"
            )
        } else {
            format!("{SUSPENSION_SELECT} WHERE s.guild_id=?1 ORDER BY s.created_at,s.discord_id")
        };
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map(params![guild_id], suspension_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn record_event(
        &self,
        request: RecordModerationEventRequest<'_>,
    ) -> Result<ModerationEvent, ModerationRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let event = insert_event(&transaction, request)?;
        transaction.commit()?;
        Ok(event)
    }

    pub fn history(
        &self,
        guild_id: Option<i64>,
        discord_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ModerationEvent>, ModerationRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let (sql, values): (String, Vec<i64>) = if let Some(discord_id) = discord_id {
            (
                format!(
                    "{EVENT_SELECT} WHERE guild_id=?1 AND discord_id=?2 ORDER BY event_id DESC LIMIT ?3 OFFSET ?4"
                ),
                vec![guild_id, discord_id, limit, offset],
            )
        } else {
            (
                format!(
                    "{EVENT_SELECT} WHERE guild_id=?1 ORDER BY event_id DESC LIMIT ?2 OFFSET ?3"
                ),
                vec![guild_id, limit, offset],
            )
        };
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map(rusqlite::params_from_iter(values), event_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn completion_events_for_match(
        &self,
        guild_id: Option<i64>,
        match_id: i64,
    ) -> Result<Vec<ModerationEvent>, ModerationRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut statement = connection.prepare(&format!(
            "{EVENT_SELECT} WHERE guild_id=?1 AND match_id=?2
             AND event_type IN ('complete','lowprio_complete') ORDER BY event_id"
        ))?;
        statement
            .query_map(params![guild_id, match_id], event_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn progress_completed_match(
        &self,
        completed: CompletedMatch<'_>,
    ) -> Result<ProgressMatchOutcome, ModerationRepositoryError> {
        let guild_id = Self::normalize_guild_id(completed.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(&format!(
            "{SUSPENSION_SELECT} WHERE s.guild_id=?1 AND s.active=1
             AND s.matches_remaining>0 AND ?2>s.pending_match_watermark
             AND (s.scope='all' OR s.scope=?3)
             AND (s.completion!='either' OR s.expires_at IS NULL OR s.expires_at>?4)
             ORDER BY s.discord_id"
        ))?;
        let candidates = statement
            .query_map(
                params![
                    guild_id,
                    completed.pending_match_id,
                    completed.lobby_kind,
                    completed.recorded_at
                ],
                suspension_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut progressed_discord_ids = Vec::new();
        let mut completed_events = Vec::new();
        for state in candidates {
            let remaining = state.matches_remaining.unwrap_or(0).saturating_sub(1);
            let completes = remaining == 0
                && (matches!(
                    state.completion,
                    SuspensionCompletion::Matches | SuspensionCompletion::Either
                ) || (state.completion == SuspensionCompletion::Both
                    && state
                        .expires_at
                        .is_some_and(|expires| expires <= completed.recorded_at)));
            let changed = transaction.execute(
                "UPDATE lobby_suspensions
                 SET matches_remaining=?1,active=CASE WHEN ?2 THEN 0 ELSE active END,
                     revision=revision+1,updated_at=?3
                 WHERE guild_id=?4 AND discord_id=?5 AND active=1 AND revision=?6",
                params![
                    remaining,
                    completes,
                    completed.recorded_at,
                    guild_id,
                    state.discord_id,
                    state.revision
                ],
            )?;
            if changed != 1 {
                continue;
            }
            progressed_discord_ids.push(state.discord_id);
            if completes {
                completed_events.push(insert_event(
                    &transaction,
                    RecordModerationEventRequest {
                        discord_id: state.discord_id,
                        guild_id: Some(guild_id),
                        event_type: ModerationEventType::Complete,
                        source: ModerationSource::System,
                        actor_id: None,
                        reason: Some("Suspension requirements completed"),
                        scope: Some(state.scope),
                        completion: Some(state.completion),
                        expires_at: state.expires_at,
                        matches_required: state.matches_required,
                        matches_remaining: Some(0),
                        pending_match_watermark: Some(state.pending_match_watermark),
                        wins_required: None,
                        wins_remaining: None,
                        match_id: Some(completed.match_id),
                        now: completed.recorded_at,
                    },
                )?);
            }
        }
        transaction.commit()?;
        Ok(ProgressMatchOutcome {
            progressed_discord_ids,
            completed_events,
        })
    }
}

const SUSPENSION_SELECT: &str =
    "SELECT s.discord_id,s.guild_id,s.scope,s.completion,s.reason,s.source,
            s.actor_id,s.expires_at,s.matches_required,s.matches_remaining,
            s.pending_match_watermark,s.revision,s.active,s.created_at,s.updated_at
     FROM lobby_suspensions s";

const EVENT_SELECT: &str = "SELECT event_id,guild_id,discord_id,event_type,source,actor_id,reason,
            scope,completion,expires_at,matches_required,matches_remaining,
            pending_match_watermark,wins_required,wins_remaining,match_id,created_at
     FROM moderation_events";

fn query_suspension(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<LobbySuspension>, ModerationRepositoryError> {
    connection
        .query_row(
            &format!("{SUSPENSION_SELECT} WHERE s.discord_id=?1 AND s.guild_id=?2"),
            params![discord_id, guild_id],
            suspension_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn suspension_from_row(row: &rusqlite::Row<'_>) -> Result<LobbySuspension, rusqlite::Error> {
    let scope = row.get::<_, String>(2)?;
    let completion = row.get::<_, String>(3)?;
    let source = row.get::<_, String>(5)?;
    Ok(LobbySuspension {
        discord_id: row.get(0)?,
        guild_id: row.get(1)?,
        scope: SuspensionScope::parse(&scope).ok_or_else(|| invalid_value(2, &scope))?,
        completion: SuspensionCompletion::parse(&completion)
            .ok_or_else(|| invalid_value(3, &completion))?,
        reason: row.get(4)?,
        source: ModerationSource::parse(&source).ok_or_else(|| invalid_value(5, &source))?,
        actor_id: row.get(6)?,
        expires_at: row.get(7)?,
        matches_required: row.get(8)?,
        matches_remaining: row.get(9)?,
        pending_match_watermark: row.get(10)?,
        revision: row.get(11)?,
        active: row.get::<_, i64>(12)? != 0,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

/// Append one audit row inside an existing transaction and return its id.
///
/// Shared with [`crate::low_priority`] so a low-priority state change and its
/// audit event commit together instead of in two transactions.
pub(crate) fn insert_event_row(
    transaction: &Transaction<'_>,
    request: RecordModerationEventRequest<'_>,
) -> Result<i64, rusqlite::Error> {
    let guild_id = ModerationRepository::normalize_guild_id(request.guild_id);
    transaction.execute(
        "INSERT INTO moderation_events (
             guild_id,discord_id,event_type,source,actor_id,reason,scope,completion,
             expires_at,matches_required,matches_remaining,pending_match_watermark,
             wins_required,wins_remaining,match_id,created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        params![
            guild_id,
            request.discord_id,
            request.event_type.name(),
            request.source.name(),
            request.actor_id,
            request.reason,
            request.scope.map(SuspensionScope::name),
            request.completion.map(SuspensionCompletion::name),
            request.expires_at,
            request.matches_required,
            request.matches_remaining,
            request.pending_match_watermark,
            request.wins_required,
            request.wins_remaining,
            request.match_id,
            request.now,
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn insert_event(
    transaction: &Transaction<'_>,
    request: RecordModerationEventRequest<'_>,
) -> Result<ModerationEvent, ModerationRepositoryError> {
    let event_id = insert_event_row(transaction, request)?;
    transaction
        .query_row(
            &format!("{EVENT_SELECT} WHERE event_id=?1"),
            params![event_id],
            event_from_row,
        )
        .map_err(Into::into)
}

fn event_from_row(row: &rusqlite::Row<'_>) -> Result<ModerationEvent, rusqlite::Error> {
    let event_type = row.get::<_, String>(3)?;
    let source = row.get::<_, String>(4)?;
    let scope = row.get::<_, Option<String>>(7)?;
    let completion = row.get::<_, Option<String>>(8)?;
    Ok(ModerationEvent {
        event_id: row.get(0)?,
        guild_id: row.get(1)?,
        discord_id: row.get(2)?,
        event_type: ModerationEventType::parse(&event_type)
            .ok_or_else(|| invalid_value(3, &event_type))?,
        source: ModerationSource::parse(&source).ok_or_else(|| invalid_value(4, &source))?,
        actor_id: row.get(5)?,
        reason: row.get(6)?,
        scope: scope
            .as_deref()
            .map(SuspensionScope::parse)
            .transpose_option()
            .ok_or_else(|| invalid_value(7, scope.as_deref().unwrap_or("")))?,
        completion: completion
            .as_deref()
            .map(SuspensionCompletion::parse)
            .transpose_option()
            .ok_or_else(|| invalid_value(8, completion.as_deref().unwrap_or("")))?,
        expires_at: row.get(9)?,
        matches_required: row.get(10)?,
        matches_remaining: row.get(11)?,
        pending_match_watermark: row.get(12)?,
        wins_required: row.get(13)?,
        wins_remaining: row.get(14)?,
        match_id: row.get(15)?,
        created_at: row.get(16)?,
    })
}

trait TransposeOption<T> {
    fn transpose_option(self) -> Option<Option<T>>;
}

impl<T> TransposeOption<T> for Option<Option<T>> {
    fn transpose_option(self) -> Option<Option<T>> {
        match self {
            None => Some(None),
            Some(Some(value)) => Some(Some(value)),
            Some(None) => None,
        }
    }
}

fn invalid_value(index: usize, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid moderation value: {value}"),
        )
        .into(),
    )
}

fn query_watermark(connection: &Connection, guild_id: i64) -> Result<i64, rusqlite::Error> {
    connection.query_row(
        "SELECT COALESCE(MAX(pending_match_id),0) FROM (
             SELECT pending_match_id FROM pending_matches WHERE guild_id=?1
             UNION ALL
             SELECT pending_match_id FROM matches
             WHERE guild_id=?1 AND pending_match_id IS NOT NULL
         )",
        params![guild_id],
        |row| row.get(0),
    )
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            params![table],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn trigger_exists(connection: &Connection, trigger: &str) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1",
            params![trigger],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn table_columns(
    connection: &Connection,
    table: &str,
) -> Result<BTreeSet<String>, rusqlite::Error> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect()
}

#[cfg(test)]
mod tests;
