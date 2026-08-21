//! Discord-neutral moderation policy over an existing-schema persistence port.

use cama_db::moderation::{
    CloseSuspensionOutcome, CloseSuspensionRequest, CompletedMatch, LobbySuspension,
    LowPriorityMigrationState, ModerationEvent, ModerationEventType, ModerationRepository,
    ModerationRepositoryError, ModerationSchemaContract, ModerationSource, ProgressMatchOutcome,
    RecordModerationEventRequest, SaveSuspensionOutcome, SaveSuspensionRequest,
    SuspensionCompletion, SuspensionScope,
};
use thiserror::Error;

pub const MIN_DURATION_SECONDS: i64 = 5 * 60;
pub const MAX_DURATION_SECONDS: i64 = 365 * 24 * 60 * 60;
pub const MIN_MATCHES: i64 = 1;
pub const MAX_MATCHES: i64 = 20;

pub trait ModerationPort {
    type Error;

    fn schema_contract(&self) -> Result<ModerationSchemaContract, Self::Error>;
    fn low_priority_migration_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<LowPriorityMigrationState>, Self::Error>;
    fn pending_match_watermark(&self, guild_id: Option<i64>) -> Result<i64, Self::Error>;
    fn suspension(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<LobbySuspension>, Self::Error>;
    fn save_suspension(
        &self,
        request: SaveSuspensionRequest<'_>,
    ) -> Result<SaveSuspensionOutcome, Self::Error>;
    fn close_suspension(
        &self,
        request: CloseSuspensionRequest<'_>,
    ) -> Result<CloseSuspensionOutcome, Self::Error>;
    fn list_suspensions(
        &self,
        guild_id: Option<i64>,
        active_only: bool,
    ) -> Result<Vec<LobbySuspension>, Self::Error>;
    fn record_event(
        &self,
        request: RecordModerationEventRequest<'_>,
    ) -> Result<ModerationEvent, Self::Error>;
    fn history(
        &self,
        guild_id: Option<i64>,
        discord_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ModerationEvent>, Self::Error>;
    fn progress_completed_match(
        &self,
        completed: CompletedMatch<'_>,
    ) -> Result<ProgressMatchOutcome, Self::Error>;
}

impl ModerationPort for ModerationRepository {
    type Error = ModerationRepositoryError;

    fn schema_contract(&self) -> Result<ModerationSchemaContract, Self::Error> {
        ModerationRepository::schema_contract(self)
    }

    fn low_priority_migration_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<LowPriorityMigrationState>, Self::Error> {
        ModerationRepository::low_priority_migration_state(self, discord_id, guild_id)
    }

    fn pending_match_watermark(&self, guild_id: Option<i64>) -> Result<i64, Self::Error> {
        ModerationRepository::pending_match_watermark(self, guild_id)
    }

    fn suspension(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<LobbySuspension>, Self::Error> {
        ModerationRepository::suspension(self, discord_id, guild_id)
    }

    fn save_suspension(
        &self,
        request: SaveSuspensionRequest<'_>,
    ) -> Result<SaveSuspensionOutcome, Self::Error> {
        ModerationRepository::save_suspension(self, request)
    }

    fn close_suspension(
        &self,
        request: CloseSuspensionRequest<'_>,
    ) -> Result<CloseSuspensionOutcome, Self::Error> {
        ModerationRepository::close_suspension(self, request)
    }

    fn list_suspensions(
        &self,
        guild_id: Option<i64>,
        active_only: bool,
    ) -> Result<Vec<LobbySuspension>, Self::Error> {
        ModerationRepository::list_suspensions(self, guild_id, active_only)
    }

    fn record_event(
        &self,
        request: RecordModerationEventRequest<'_>,
    ) -> Result<ModerationEvent, Self::Error> {
        ModerationRepository::record_event(self, request)
    }

    fn history(
        &self,
        guild_id: Option<i64>,
        discord_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ModerationEvent>, Self::Error> {
        ModerationRepository::history(self, guild_id, discord_id, limit, offset)
    }

    fn progress_completed_match(
        &self,
        completed: CompletedMatch<'_>,
    ) -> Result<ProgressMatchOutcome, Self::Error> {
        ModerationRepository::progress_completed_match(self, completed)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CreateSuspension<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub actor_id: i64,
    pub reason: &'a str,
    pub scope: SuspensionScope,
    pub completion: SuspensionCompletion,
    pub expires_at: Option<i64>,
    pub matches: Option<i64>,
    pub source: ModerationSource,
    pub replace: bool,
    pub pending_match_watermark: Option<i64>,
    pub now: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct RecordKick<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub actor_id: i64,
    pub lobby_kind: SuspensionScope,
    pub reason: Option<&'a str>,
    pub source: ModerationSource,
    pub now: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct RecordLowPriorityEvent<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub event_type: ModerationEventType,
    pub wins_required: i64,
    pub wins_remaining: i64,
    pub actor_id: Option<i64>,
    pub reason: Option<&'a str>,
    pub match_id: Option<i64>,
    pub now: i64,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ModerationPolicyError {
    #[error("Duration is required (for example: 30m, 2h, 3d, or 1w)")]
    DurationRequired,
    #[error("Invalid duration; use units s, m, h, d, or w")]
    InvalidDuration,
    #[error("Duration must be between 5 minutes and 365 days")]
    DurationOutOfBounds,
    #[error("matches must be between 1 and 20")]
    MatchesOutOfBounds,
    #[error("completion requirements do not match the selected mode")]
    CompletionRequirements,
    #[error("A moderation reason is required")]
    ReasonRequired,
    #[error("Player already has an active lobby suspension")]
    ActiveSuspensionExists,
    #[error("A kick must identify the concrete lobby kind")]
    ConcreteLobbyRequired,
    #[error("invalid low-priority moderation event")]
    InvalidLowPriorityEvent,
    #[error("wins_remaining cannot exceed wins_required")]
    InvalidWinProgress,
}

#[derive(Debug)]
pub enum ModerationServiceError<E> {
    Port(E),
    Policy(ModerationPolicyError),
}

#[derive(Clone, Debug)]
pub struct ModerationService<P> {
    port: P,
}

impl<P: ModerationPort> ModerationService<P> {
    #[must_use]
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    pub fn create_suspension(
        &self,
        request: CreateSuspension<'_>,
    ) -> Result<LobbySuspension, ModerationServiceError<P::Error>> {
        validate_create(request).map_err(ModerationServiceError::Policy)?;
        if !request.replace
            && self
                .get_active_suspension(request.discord_id, request.guild_id, None, request.now)?
                .is_some()
        {
            return Err(ModerationServiceError::Policy(
                ModerationPolicyError::ActiveSuspensionExists,
            ));
        }
        match self
            .port
            .save_suspension(SaveSuspensionRequest {
                discord_id: request.discord_id,
                guild_id: request.guild_id,
                scope: request.scope,
                completion: request.completion,
                expires_at: request.expires_at,
                matches_required: request.matches,
                pending_match_watermark: request.pending_match_watermark,
                reason: request.reason.trim(),
                source: request.source,
                actor_id: request.actor_id,
                replace: request.replace,
                now: request.now,
            })
            .map_err(ModerationServiceError::Port)?
        {
            SaveSuspensionOutcome::Saved(state) => Ok(state),
            SaveSuspensionOutcome::ActiveExists(_) => Err(ModerationServiceError::Policy(
                ModerationPolicyError::ActiveSuspensionExists,
            )),
        }
    }

    pub fn get_active_suspension(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        lobby_kind: Option<SuspensionScope>,
        now: i64,
    ) -> Result<Option<LobbySuspension>, ModerationServiceError<P::Error>> {
        for _ in 0..3 {
            let state = self
                .port
                .suspension(discord_id, guild_id)
                .map_err(ModerationServiceError::Port)?;
            let Some(state) = state.filter(|state| state.active) else {
                return Ok(None);
            };
            if state.is_active_at(now) {
                return Ok(state.applies_to(lobby_kind).then_some(state));
            }
            let outcome = self
                .port
                .close_suspension(CloseSuspensionRequest {
                    discord_id,
                    guild_id,
                    event_type: ModerationEventType::Complete,
                    source: ModerationSource::System,
                    actor_id: None,
                    reason: Some("Suspension requirements completed"),
                    expected_revision: state.revision,
                    now,
                })
                .map_err(ModerationServiceError::Port)?;
            if matches!(outcome, CloseSuspensionOutcome::Closed(_)) {
                return Ok(None);
            }
        }
        let latest = self
            .port
            .suspension(discord_id, guild_id)
            .map_err(ModerationServiceError::Port)?;
        Ok(latest.filter(|state| state.is_active_at(now) && state.applies_to(lobby_kind)))
    }

    pub fn lift_suspension(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        actor_id: i64,
        reason: &str,
        now: i64,
    ) -> Result<Option<LobbySuspension>, ModerationServiceError<P::Error>> {
        if reason.trim().is_empty() {
            return Err(ModerationServiceError::Policy(
                ModerationPolicyError::ReasonRequired,
            ));
        }
        let Some(active) = self.get_active_suspension(discord_id, guild_id, None, now)? else {
            return Ok(None);
        };
        let outcome = self
            .port
            .close_suspension(CloseSuspensionRequest {
                discord_id,
                guild_id,
                event_type: ModerationEventType::Lift,
                source: ModerationSource::Admin,
                actor_id: Some(actor_id),
                reason: Some(reason.trim()),
                expected_revision: active.revision,
                now,
            })
            .map_err(ModerationServiceError::Port)?;
        Ok(match outcome {
            CloseSuspensionOutcome::Closed(closed) => Some(closed),
            CloseSuspensionOutcome::ConflictOrMissing => None,
        })
    }

    pub fn list_active_suspensions(
        &self,
        guild_id: Option<i64>,
        lobby_kind: Option<SuspensionScope>,
        now: i64,
    ) -> Result<Vec<LobbySuspension>, ModerationServiceError<P::Error>> {
        let states = self
            .port
            .list_suspensions(guild_id, true)
            .map_err(ModerationServiceError::Port)?;
        let mut active = Vec::new();
        for state in states {
            if let Some(current) =
                self.get_active_suspension(state.discord_id, guild_id, lobby_kind, now)?
            {
                active.push(current);
            }
        }
        Ok(active)
    }

    pub fn history(
        &self,
        guild_id: Option<i64>,
        discord_id: Option<i64>,
    ) -> Result<Vec<ModerationEvent>, P::Error> {
        self.port.history(guild_id, discord_id, 50, 0)
    }

    pub fn record_kick(
        &self,
        request: RecordKick<'_>,
    ) -> Result<ModerationEvent, ModerationServiceError<P::Error>> {
        if request.lobby_kind == SuspensionScope::All {
            return Err(ModerationServiceError::Policy(
                ModerationPolicyError::ConcreteLobbyRequired,
            ));
        }
        self.port
            .record_event(RecordModerationEventRequest {
                discord_id: request.discord_id,
                guild_id: request.guild_id,
                event_type: ModerationEventType::Kick,
                source: request.source,
                actor_id: Some(request.actor_id),
                reason: request
                    .reason
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty()),
                scope: Some(request.lobby_kind),
                completion: None,
                expires_at: None,
                matches_required: None,
                matches_remaining: None,
                pending_match_watermark: None,
                wins_required: None,
                wins_remaining: None,
                match_id: None,
                now: request.now,
            })
            .map_err(ModerationServiceError::Port)
    }

    pub fn record_low_priority_event(
        &self,
        request: RecordLowPriorityEvent<'_>,
    ) -> Result<ModerationEvent, ModerationServiceError<P::Error>> {
        validate_low_priority_event(
            request.event_type,
            request.wins_required,
            request.wins_remaining,
        )
        .map_err(ModerationServiceError::Policy)?;
        self.port
            .record_event(RecordModerationEventRequest {
                discord_id: request.discord_id,
                guild_id: request.guild_id,
                event_type: request.event_type,
                source: if request.event_type == ModerationEventType::LowprioComplete {
                    ModerationSource::System
                } else {
                    ModerationSource::Admin
                },
                actor_id: request.actor_id,
                reason: request.reason,
                scope: None,
                completion: None,
                expires_at: None,
                matches_required: None,
                matches_remaining: None,
                pending_match_watermark: None,
                wins_required: Some(request.wins_required),
                wins_remaining: Some(request.wins_remaining),
                match_id: request.match_id,
                now: request.now,
            })
            .map_err(ModerationServiceError::Port)
    }
}

/// Policy every low-priority moderation event must satisfy before it is
/// written, whether it is written on its own or inside the
/// `low_priority_state` transaction that now records it atomically.
pub fn validate_low_priority_event(
    event_type: ModerationEventType,
    wins_required: i64,
    wins_remaining: i64,
) -> Result<(), ModerationPolicyError> {
    if !matches!(
        event_type,
        ModerationEventType::LowprioAssign
            | ModerationEventType::LowprioReplace
            | ModerationEventType::LowprioClear
            | ModerationEventType::LowprioComplete
    ) || !(1..=20).contains(&wins_required)
        || !(0..=20).contains(&wins_remaining)
    {
        return Err(ModerationPolicyError::InvalidLowPriorityEvent);
    }
    if wins_remaining > wins_required {
        return Err(ModerationPolicyError::InvalidWinProgress);
    }
    Ok(())
}

pub fn parse_duration_seconds(value: &str) -> Result<i64, ModerationPolicyError> {
    let raw = value.trim().as_bytes();
    if raw.is_empty() {
        return Err(ModerationPolicyError::DurationRequired);
    }
    let mut position = 0;
    let mut total = 0_i64;
    let mut matched = false;
    while position < raw.len() {
        while position < raw.len() && raw[position].is_ascii_whitespace() {
            position += 1;
        }
        let start = position;
        while position < raw.len() && raw[position].is_ascii_digit() {
            position += 1;
        }
        if start == position || position >= raw.len() {
            return Err(ModerationPolicyError::InvalidDuration);
        }
        let amount = std::str::from_utf8(&raw[start..position])
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or(ModerationPolicyError::InvalidDuration)?;
        while position < raw.len() && raw[position].is_ascii_whitespace() {
            position += 1;
        }
        if position >= raw.len() {
            return Err(ModerationPolicyError::InvalidDuration);
        }
        let multiplier = match raw[position].to_ascii_lowercase() {
            b's' => 1,
            b'm' => 60,
            b'h' => 3_600,
            b'd' => 86_400,
            b'w' => 604_800,
            _ => return Err(ModerationPolicyError::InvalidDuration),
        };
        position += 1;
        total = total
            .checked_add(
                amount
                    .checked_mul(multiplier)
                    .ok_or(ModerationPolicyError::DurationOutOfBounds)?,
            )
            .ok_or(ModerationPolicyError::DurationOutOfBounds)?;
        matched = true;
    }
    if !matched {
        return Err(ModerationPolicyError::InvalidDuration);
    }
    if !(MIN_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&total) {
        return Err(ModerationPolicyError::DurationOutOfBounds);
    }
    Ok(total)
}

pub fn parse_expiry(value: &str, now: i64) -> Result<i64, ModerationPolicyError> {
    now.checked_add(parse_duration_seconds(value)?)
        .ok_or(ModerationPolicyError::DurationOutOfBounds)
}

pub fn schema_contract_complete(contract: &ModerationSchemaContract) -> bool {
    let suspension = [
        "scope",
        "completion",
        "expires_at",
        "matches_required",
        "matches_remaining",
        "pending_match_watermark",
        "revision",
        "active",
    ];
    let events = [
        "matches_remaining",
        "wins_required",
        "wins_remaining",
        "match_id",
    ];
    contract.has_lobby_suspensions
        && contract.has_moderation_events
        && contract.matches_has_lobby_kind
        && suspension
            .iter()
            .all(|column| contract.suspension_columns.contains(*column))
        && events
            .iter()
            .all(|column| contract.event_columns.contains(*column))
        && ["wins_required", "start_pending_match_id"]
            .iter()
            .all(|column| contract.low_priority_columns.contains(*column))
        && contract.append_only_update_trigger
        && contract.append_only_delete_trigger
}

fn validate_create(request: CreateSuspension<'_>) -> Result<(), ModerationPolicyError> {
    if request.reason.trim().is_empty() {
        return Err(ModerationPolicyError::ReasonRequired);
    }
    let needs_time = matches!(
        request.completion,
        SuspensionCompletion::Time | SuspensionCompletion::Either | SuspensionCompletion::Both
    );
    let needs_matches = matches!(
        request.completion,
        SuspensionCompletion::Matches | SuspensionCompletion::Either | SuspensionCompletion::Both
    );
    if needs_time {
        let duration = request
            .expires_at
            .and_then(|expires| expires.checked_sub(request.now))
            .ok_or(ModerationPolicyError::CompletionRequirements)?;
        if !(MIN_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&duration) {
            return Err(ModerationPolicyError::DurationOutOfBounds);
        }
    } else if request.expires_at.is_some() {
        return Err(ModerationPolicyError::CompletionRequirements);
    }
    if needs_matches {
        if !request
            .matches
            .is_some_and(|matches| (MIN_MATCHES..=MAX_MATCHES).contains(&matches))
        {
            return Err(ModerationPolicyError::MatchesOutOfBounds);
        }
    } else if request.matches.is_some() {
        return Err(ModerationPolicyError::CompletionRequirements);
    }
    Ok(())
}

#[cfg(test)]
#[path = "moderation/tests.rs"]
mod tests;
