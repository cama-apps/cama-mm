use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use super::*;

const NOW: i64 = 2_000_000_000;
const GUILD: i64 = 98_765;
const OTHER_GUILD: i64 = 98_766;

#[derive(Clone, Debug)]
struct MemoryPort {
    state: Arc<Mutex<MemoryState>>,
}

#[derive(Clone, Debug)]
struct MemoryState {
    suspensions: BTreeMap<(i64, i64), LobbySuspension>,
    events: Vec<ModerationEvent>,
    watermarks: BTreeMap<i64, i64>,
    next_event_id: i64,
    replace_before_next_close: Option<SaveSuspensionOwned>,
    contract: ModerationSchemaContract,
    low_priority: Option<LowPriorityMigrationState>,
}

#[derive(Clone, Debug)]
struct SaveSuspensionOwned {
    discord_id: i64,
    guild_id: i64,
    scope: SuspensionScope,
    completion: SuspensionCompletion,
    expires_at: Option<i64>,
    matches_required: Option<i64>,
    reason: String,
    source: ModerationSource,
    actor_id: i64,
    now: i64,
}

impl MemoryPort {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MemoryState {
                suspensions: BTreeMap::new(),
                events: Vec::new(),
                watermarks: BTreeMap::new(),
                next_event_id: 1,
                replace_before_next_close: None,
                contract: complete_contract(),
                low_priority: Some(LowPriorityMigrationState {
                    wins_required: 3,
                    wins_remaining: 2,
                    start_pending_match_id: None,
                    reason: Some("legacy".to_owned()),
                }),
            })),
        }
    }

    fn state(&self) -> MemoryState {
        self.state.lock().expect("memory moderation lock").clone()
    }

    fn set_watermark(&self, guild_id: i64, watermark: i64) {
        self.state
            .lock()
            .expect("memory moderation lock")
            .watermarks
            .insert(guild_id, watermark);
    }

    fn replace_before_next_close(&self, replacement: SaveSuspensionOwned) {
        self.state
            .lock()
            .expect("memory moderation lock")
            .replace_before_next_close = Some(replacement);
    }
}

impl ModerationPort for MemoryPort {
    type Error = Infallible;

    fn schema_contract(&self) -> Result<ModerationSchemaContract, Self::Error> {
        Ok(self.state().contract)
    }

    fn low_priority_migration_state(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
    ) -> Result<Option<LowPriorityMigrationState>, Self::Error> {
        Ok(self.state().low_priority)
    }

    fn pending_match_watermark(&self, guild_id: Option<i64>) -> Result<i64, Self::Error> {
        Ok(self
            .state()
            .watermarks
            .get(&guild_id.unwrap_or(0))
            .copied()
            .unwrap_or(0))
    }

    fn suspension(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<LobbySuspension>, Self::Error> {
        Ok(self
            .state()
            .suspensions
            .get(&(guild_id.unwrap_or(0), discord_id))
            .cloned())
    }

    fn save_suspension(
        &self,
        request: SaveSuspensionRequest<'_>,
    ) -> Result<SaveSuspensionOutcome, Self::Error> {
        let guild_id = request.guild_id.unwrap_or(0);
        let mut state = self.state.lock().expect("memory moderation lock");
        let key = (guild_id, request.discord_id);
        let existing = state.suspensions.get(&key).cloned();
        let replacing = existing.as_ref().is_some_and(|state| state.active);
        if replacing && !request.replace {
            return Ok(SaveSuspensionOutcome::ActiveExists(
                existing.expect("active exists"),
            ));
        }
        let revision = existing.map_or(1, |state| state.revision + 1);
        let watermark = request
            .pending_match_watermark
            .unwrap_or_else(|| state.watermarks.get(&guild_id).copied().unwrap_or(0));
        let saved = LobbySuspension {
            discord_id: request.discord_id,
            guild_id,
            scope: request.scope,
            completion: request.completion,
            reason: request.reason.to_owned(),
            source: request.source,
            actor_id: request.actor_id,
            expires_at: request.expires_at,
            matches_required: request.matches_required,
            matches_remaining: request.matches_required,
            pending_match_watermark: watermark,
            revision,
            active: true,
            created_at: request.now,
            updated_at: request.now,
        };
        state.suspensions.insert(key, saved.clone());
        push_event(
            &mut state,
            EventFields {
                guild_id,
                discord_id: request.discord_id,
                event_type: if replacing {
                    ModerationEventType::Replace
                } else {
                    ModerationEventType::Suspend
                },
                source: request.source,
                actor_id: Some(request.actor_id),
                reason: Some(request.reason.to_owned()),
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
        );
        Ok(SaveSuspensionOutcome::Saved(saved))
    }

    fn close_suspension(
        &self,
        request: CloseSuspensionRequest<'_>,
    ) -> Result<CloseSuspensionOutcome, Self::Error> {
        let guild_id = request.guild_id.unwrap_or(0);
        let mut state = self.state.lock().expect("memory moderation lock");
        if let Some(replacement) = state.replace_before_next_close.take() {
            replace_direct(&mut state, replacement);
        }
        let key = (guild_id, request.discord_id);
        let Some(current) = state.suspensions.get(&key).cloned() else {
            return Ok(CloseSuspensionOutcome::ConflictOrMissing);
        };
        if !current.active || current.revision != request.expected_revision {
            return Ok(CloseSuspensionOutcome::ConflictOrMissing);
        }
        let mut closed = current.clone();
        closed.active = false;
        closed.revision += 1;
        closed.updated_at = request.now;
        state.suspensions.insert(key, closed.clone());
        push_event(
            &mut state,
            EventFields {
                guild_id,
                discord_id: request.discord_id,
                event_type: request.event_type,
                source: request.source,
                actor_id: request.actor_id,
                reason: request.reason.map(str::to_owned),
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
        );
        Ok(CloseSuspensionOutcome::Closed(closed))
    }

    fn list_suspensions(
        &self,
        guild_id: Option<i64>,
        active_only: bool,
    ) -> Result<Vec<LobbySuspension>, Self::Error> {
        let guild_id = guild_id.unwrap_or(0);
        Ok(self
            .state()
            .suspensions
            .into_values()
            .filter(|state| state.guild_id == guild_id && (!active_only || state.active))
            .collect())
    }

    fn record_event(
        &self,
        request: RecordModerationEventRequest<'_>,
    ) -> Result<ModerationEvent, Self::Error> {
        let mut state = self.state.lock().expect("memory moderation lock");
        Ok(push_event(
            &mut state,
            EventFields {
                guild_id: request.guild_id.unwrap_or(0),
                discord_id: request.discord_id,
                event_type: request.event_type,
                source: request.source,
                actor_id: request.actor_id,
                reason: request.reason.map(str::to_owned),
                scope: request.scope,
                completion: request.completion,
                expires_at: request.expires_at,
                matches_required: request.matches_required,
                matches_remaining: request.matches_remaining,
                pending_match_watermark: request.pending_match_watermark,
                wins_required: request.wins_required,
                wins_remaining: request.wins_remaining,
                match_id: request.match_id,
                now: request.now,
            },
        ))
    }

    fn history(
        &self,
        guild_id: Option<i64>,
        discord_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ModerationEvent>, Self::Error> {
        Ok(self
            .state()
            .events
            .into_iter()
            .rev()
            .filter(|event| {
                event.guild_id == guild_id.unwrap_or(0)
                    && discord_id.is_none_or(|discord_id| event.discord_id == discord_id)
            })
            .skip(offset as usize)
            .take(limit as usize)
            .collect())
    }

    fn progress_completed_match(
        &self,
        completed: CompletedMatch<'_>,
    ) -> Result<ProgressMatchOutcome, Self::Error> {
        let guild_id = completed.guild_id.unwrap_or(0);
        let mut state = self.state.lock().expect("memory moderation lock");
        let keys = state
            .suspensions
            .iter()
            .filter_map(|(key, suspension)| {
                (suspension.guild_id == guild_id
                    && suspension.active
                    && suspension.matches_remaining.is_some_and(|value| value > 0)
                    && completed.pending_match_id > suspension.pending_match_watermark
                    && (suspension.scope == SuspensionScope::All
                        || suspension.scope.name() == completed.lobby_kind)
                    && !(suspension.completion == SuspensionCompletion::Either
                        && suspension
                            .expires_at
                            .is_some_and(|expiry| expiry <= completed.recorded_at)))
                .then_some(*key)
            })
            .collect::<Vec<_>>();
        let mut progressed_discord_ids = Vec::new();
        let mut completed_events = Vec::new();
        for key in keys {
            let suspension = state.suspensions.get_mut(&key).expect("candidate");
            let remaining = suspension.matches_remaining.unwrap_or(0) - 1;
            suspension.matches_remaining = Some(remaining);
            suspension.revision += 1;
            suspension.updated_at = completed.recorded_at;
            let completes = remaining == 0
                && (matches!(
                    suspension.completion,
                    SuspensionCompletion::Matches | SuspensionCompletion::Either
                ) || (suspension.completion == SuspensionCompletion::Both
                    && suspension
                        .expires_at
                        .is_some_and(|expiry| expiry <= completed.recorded_at)));
            if completes {
                suspension.active = false;
            }
            let snapshot = suspension.clone();
            progressed_discord_ids.push(snapshot.discord_id);
            if completes {
                completed_events.push(push_event(
                    &mut state,
                    EventFields {
                        guild_id,
                        discord_id: snapshot.discord_id,
                        event_type: ModerationEventType::Complete,
                        source: ModerationSource::System,
                        actor_id: None,
                        reason: Some("Suspension requirements completed".to_owned()),
                        scope: Some(snapshot.scope),
                        completion: Some(snapshot.completion),
                        expires_at: snapshot.expires_at,
                        matches_required: snapshot.matches_required,
                        matches_remaining: Some(0),
                        pending_match_watermark: Some(snapshot.pending_match_watermark),
                        wins_required: None,
                        wins_remaining: None,
                        match_id: Some(completed.match_id),
                        now: completed.recorded_at,
                    },
                ));
            }
        }
        Ok(ProgressMatchOutcome {
            progressed_discord_ids,
            completed_events,
        })
    }
}

#[derive(Clone, Debug)]
struct EventFields {
    guild_id: i64,
    discord_id: i64,
    event_type: ModerationEventType,
    source: ModerationSource,
    actor_id: Option<i64>,
    reason: Option<String>,
    scope: Option<SuspensionScope>,
    completion: Option<SuspensionCompletion>,
    expires_at: Option<i64>,
    matches_required: Option<i64>,
    matches_remaining: Option<i64>,
    pending_match_watermark: Option<i64>,
    wins_required: Option<i64>,
    wins_remaining: Option<i64>,
    match_id: Option<i64>,
    now: i64,
}

fn push_event(state: &mut MemoryState, fields: EventFields) -> ModerationEvent {
    let event = ModerationEvent {
        event_id: state.next_event_id,
        guild_id: fields.guild_id,
        discord_id: fields.discord_id,
        event_type: fields.event_type,
        source: fields.source,
        actor_id: fields.actor_id,
        reason: fields.reason,
        scope: fields.scope,
        completion: fields.completion,
        expires_at: fields.expires_at,
        matches_required: fields.matches_required,
        matches_remaining: fields.matches_remaining,
        pending_match_watermark: fields.pending_match_watermark,
        wins_required: fields.wins_required,
        wins_remaining: fields.wins_remaining,
        match_id: fields.match_id,
        created_at: fields.now,
    };
    state.next_event_id += 1;
    state.events.push(event.clone());
    event
}

fn replace_direct(state: &mut MemoryState, replacement: SaveSuspensionOwned) {
    let key = (replacement.guild_id, replacement.discord_id);
    let revision = state
        .suspensions
        .get(&key)
        .map_or(1, |current| current.revision + 1);
    let watermark = state
        .watermarks
        .get(&replacement.guild_id)
        .copied()
        .unwrap_or(0);
    state.suspensions.insert(
        key,
        LobbySuspension {
            discord_id: replacement.discord_id,
            guild_id: replacement.guild_id,
            scope: replacement.scope,
            completion: replacement.completion,
            reason: replacement.reason.clone(),
            source: replacement.source,
            actor_id: replacement.actor_id,
            expires_at: replacement.expires_at,
            matches_required: replacement.matches_required,
            matches_remaining: replacement.matches_required,
            pending_match_watermark: watermark,
            revision,
            active: true,
            created_at: replacement.now,
            updated_at: replacement.now,
        },
    );
    push_event(
        state,
        EventFields {
            guild_id: replacement.guild_id,
            discord_id: replacement.discord_id,
            event_type: ModerationEventType::Replace,
            source: replacement.source,
            actor_id: Some(replacement.actor_id),
            reason: Some(replacement.reason),
            scope: Some(replacement.scope),
            completion: Some(replacement.completion),
            expires_at: replacement.expires_at,
            matches_required: replacement.matches_required,
            matches_remaining: replacement.matches_required,
            pending_match_watermark: Some(watermark),
            wins_required: None,
            wins_remaining: None,
            match_id: None,
            now: replacement.now,
        },
    );
}

fn complete_contract() -> ModerationSchemaContract {
    ModerationSchemaContract {
        has_lobby_suspensions: true,
        has_moderation_events: true,
        matches_has_lobby_kind: true,
        suspension_columns: [
            "scope",
            "completion",
            "expires_at",
            "matches_required",
            "matches_remaining",
            "pending_match_watermark",
            "revision",
            "active",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        event_columns: [
            "matches_remaining",
            "wins_required",
            "wins_remaining",
            "match_id",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        low_priority_columns: ["wins_required", "start_pending_match_id"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        append_only_update_trigger: true,
        append_only_delete_trigger: true,
    }
}

fn service() -> ModerationService<MemoryPort> {
    ModerationService::new(MemoryPort::new())
}

fn time_request(discord_id: i64, guild_id: i64, expires_at: i64) -> CreateSuspension<'static> {
    CreateSuspension {
        discord_id,
        guild_id: Some(guild_id),
        actor_id: 900,
        reason: "Repeated no-shows",
        scope: SuspensionScope::All,
        completion: SuspensionCompletion::Time,
        expires_at: Some(expires_at),
        matches: None,
        source: ModerationSource::Admin,
        replace: false,
        pending_match_watermark: None,
        now: NOW,
    }
}

fn assert_duration(text: &str, seconds: i64) {
    assert_eq!(parse_duration_seconds(text), Ok(seconds));
    assert_eq!(parse_expiry(text, NOW), Ok(NOW + seconds));
}

fn assert_invalid_duration(text: &str) {
    assert!(parse_duration_seconds(text).is_err());
}

#[test]
fn test_schema_creates_private_moderation_tables_and_match_kind() {
    let service = service();
    let contract = service.port().schema_contract().expect("contract");
    assert!(schema_contract_complete(&contract));
}

#[test]
fn test_low_priority_rebuild_preserves_legacy_progress() {
    let service = service();
    let state = service
        .port()
        .low_priority_migration_state(101, Some(77))
        .expect("read")
        .expect("legacy row");
    assert_eq!(state.wins_required, 3);
    assert_eq!(state.wins_remaining, 2);
    assert_eq!(state.start_pending_match_id, None);
    assert_eq!(state.reason.as_deref(), Some("legacy"));
}

#[test]
fn test_duration_parsing_5m_300() {
    assert_duration("5m", 300);
}
#[test]
fn test_duration_parsing_30m_1800() {
    assert_duration("30m", 1_800);
}
#[test]
fn test_duration_parsing_2h_7200() {
    assert_duration("2h", 7_200);
}
#[test]
fn test_duration_parsing_3d_259200() {
    assert_duration("3d", 259_200);
}
#[test]
fn test_duration_parsing_1w_604800() {
    assert_duration("1w", 604_800);
}
#[test]
fn test_duration_parsing_1d12h_129600() {
    assert_duration("1d12h", 129_600);
}
#[test]
fn test_duration_parsing_1d_12h_30m_131400() {
    assert_duration("1d 12h 30m", 131_400);
}
#[test]
fn test_duration_parsing_365d_31536000() {
    assert_duration("365d", 31_536_000);
}

#[test]
fn test_duration_parsing_rejects_empty() {
    assert_invalid_duration("");
}
#[test]
fn test_duration_parsing_rejects_2() {
    assert_invalid_duration("2");
}
#[test]
fn test_duration_parsing_rejects_tomorrow() {
    assert_invalid_duration("tomorrow");
}
#[test]
fn test_duration_parsing_rejects_1x() {
    assert_invalid_duration("1x");
}
#[test]
fn test_duration_parsing_rejects_0m() {
    assert_invalid_duration("0m");
}
#[test]
fn test_duration_parsing_rejects_299s() {
    assert_invalid_duration("299s");
}
#[test]
fn test_duration_parsing_rejects_366d() {
    assert_invalid_duration("366d");
}
#[test]
fn test_duration_parsing_rejects_1h_nope() {
    assert_invalid_duration("1h nope");
}

fn assert_bounds(request: CreateSuspension<'_>, expected: ModerationPolicyError) {
    assert!(matches!(
        service().create_suspension(request),
        Err(ModerationServiceError::Policy(error)) if error == expected
    ));
}

#[test]
fn test_suspension_plan_bounds_time_too_short() {
    assert_bounds(
        time_request(99, GUILD, NOW + 299),
        ModerationPolicyError::DurationOutOfBounds,
    );
}
#[test]
fn test_suspension_plan_bounds_time_too_long() {
    assert_bounds(
        time_request(99, GUILD, NOW + 31_536_001),
        ModerationPolicyError::DurationOutOfBounds,
    );
}
#[test]
fn test_suspension_plan_bounds_matches_zero() {
    let mut request = time_request(99, GUILD, NOW + 300);
    request.completion = SuspensionCompletion::Matches;
    request.expires_at = None;
    request.matches = Some(0);
    assert_bounds(request, ModerationPolicyError::MatchesOutOfBounds);
}
#[test]
fn test_suspension_plan_bounds_matches_twenty_one() {
    let mut request = time_request(99, GUILD, NOW + 300);
    request.completion = SuspensionCompletion::Matches;
    request.expires_at = None;
    request.matches = Some(21);
    assert_bounds(request, ModerationPolicyError::MatchesOutOfBounds);
}

#[test]
fn test_time_suspension_completes_once_and_is_audited() {
    let service = service();
    let created = service
        .create_suspension(time_request(101, GUILD, NOW + 300))
        .expect("create");
    assert_eq!(created.scope, SuspensionScope::All);
    assert!(
        service
            .get_active_suspension(101, Some(GUILD), None, NOW + 299)
            .expect("active")
            .is_some()
    );
    assert!(
        service
            .get_active_suspension(101, Some(GUILD), None, NOW + 300)
            .expect("complete")
            .is_none()
    );
    assert!(
        service
            .get_active_suspension(101, Some(GUILD), None, NOW + 301)
            .expect("still complete")
            .is_none()
    );
    let history = service.history(Some(GUILD), Some(101)).expect("history");
    assert_eq!(
        history
            .iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>(),
        vec![ModerationEventType::Complete, ModerationEventType::Suspend]
    );
}

#[test]
fn test_match_suspension_captures_watermark_and_filters_scope() {
    let service = service();
    service.port().set_watermark(GUILD, 40);
    let mut request = time_request(102, GUILD, NOW + 300);
    request.scope = SuspensionScope::Open;
    request.completion = SuspensionCompletion::Matches;
    request.expires_at = None;
    request.matches = Some(1);
    let created = service.create_suspension(request).expect("create");
    assert_eq!(created.pending_match_watermark, 40);
    service
        .port()
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: 40,
            lobby_kind: "open",
            match_id: 1,
            recorded_at: NOW,
        })
        .expect("grandfathered");
    service
        .port()
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: 41,
            lobby_kind: "lowskill",
            match_id: 2,
            recorded_at: NOW,
        })
        .expect("other scope");
    assert!(
        service
            .get_active_suspension(102, Some(GUILD), Some(SuspensionScope::Lowskill), NOW)
            .expect("filtered")
            .is_none()
    );
    let outcome = service
        .port()
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: 42,
            lobby_kind: "open",
            match_id: 77,
            recorded_at: NOW,
        })
        .expect("progress");
    assert_eq!(outcome.completed_events[0].match_id, Some(77));
    assert!(
        service
            .get_active_suspension(102, Some(GUILD), None, NOW)
            .expect("complete")
            .is_none()
    );
    let retry = service
        .port()
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: 42,
            lobby_kind: "open",
            match_id: 77,
            recorded_at: NOW,
        })
        .expect("idempotent completed-match retry");
    assert!(retry.progressed_discord_ids.is_empty());
    assert_eq!(
        service
            .history(Some(GUILD), Some(102))
            .expect("completion history")
            .into_iter()
            .filter(|event| event.event_type == ModerationEventType::Complete)
            .count(),
        1
    );
}

#[test]
fn test_either_and_both_completion_semantics() {
    let service = service();
    let mut either = time_request(201, GUILD, NOW + 300);
    either.completion = SuspensionCompletion::Either;
    either.matches = Some(2);
    service.create_suspension(either).expect("either");
    let mut both = time_request(202, GUILD, NOW + 300);
    both.completion = SuspensionCompletion::Both;
    both.matches = Some(1);
    service.create_suspension(both).expect("both");
    assert!(
        service
            .get_active_suspension(201, Some(GUILD), None, NOW + 300)
            .expect("either")
            .is_none()
    );
    assert!(
        service
            .get_active_suspension(202, Some(GUILD), None, NOW + 300)
            .expect("both")
            .is_some()
    );
    service
        .port()
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: 1,
            lobby_kind: "open",
            match_id: 8,
            recorded_at: NOW + 300,
        })
        .expect("match");
    assert!(
        service
            .get_active_suspension(202, Some(GUILD), None, NOW + 300)
            .expect("complete")
            .is_none()
    );
}

#[test]
fn test_create_replace_lift_and_guild_isolation() {
    let service = service();
    service
        .create_suspension(time_request(301, GUILD, NOW + 300))
        .expect("create");
    assert!(matches!(
        service.create_suspension(time_request(301, GUILD, NOW + 400)),
        Err(ModerationServiceError::Policy(
            ModerationPolicyError::ActiveSuspensionExists
        ))
    ));
    let mut replacement = time_request(301, GUILD, NOW + 300);
    replacement.actor_id = 901;
    replacement.reason = "Escalated";
    replacement.completion = SuspensionCompletion::Matches;
    replacement.expires_at = None;
    replacement.matches = Some(3);
    replacement.replace = true;
    let replaced = service.create_suspension(replacement).expect("replace");
    assert_eq!(replaced.reason, "Escalated");
    assert_eq!(replaced.matches_remaining, Some(3));
    assert!(
        service
            .get_active_suspension(301, Some(OTHER_GUILD), None, NOW)
            .expect("other guild")
            .is_none()
    );
    let lifted = service
        .lift_suspension(301, Some(GUILD), 902, "Appeal granted", NOW)
        .expect("lift")
        .expect("closed");
    assert!(!lifted.active);
    let history = service.history(Some(GUILD), Some(301)).expect("history");
    assert_eq!(
        history
            .iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>(),
        vec![
            ModerationEventType::Lift,
            ModerationEventType::Replace,
            ModerationEventType::Suspend
        ]
    );
}

#[test]
fn test_stale_close_cannot_close_a_concurrent_replacement() {
    let service = service();
    let original = service
        .create_suspension(time_request(302, GUILD, NOW + 300))
        .expect("original");
    let mut replacement = time_request(302, GUILD, NOW + 600);
    replacement.replace = true;
    replacement.reason = "Replacement";
    let replacement = service.create_suspension(replacement).expect("replace");
    assert_eq!(replacement.revision, original.revision + 1);
    assert_eq!(
        service
            .port()
            .close_suspension(CloseSuspensionRequest {
                discord_id: 302,
                guild_id: Some(GUILD),
                event_type: ModerationEventType::Complete,
                source: ModerationSource::System,
                actor_id: None,
                reason: Some("stale"),
                expected_revision: original.revision,
                now: NOW + 300,
            })
            .expect("close"),
        CloseSuspensionOutcome::ConflictOrMissing
    );
    let current = service
        .get_active_suspension(302, Some(GUILD), None, NOW + 300)
        .expect("active")
        .expect("replacement");
    assert_eq!(current.reason, "Replacement");
}

#[test]
fn test_active_lookup_retries_and_returns_a_racing_replacement() {
    let service = service();
    let original = service
        .create_suspension(time_request(303, GUILD, NOW + 300))
        .expect("original");
    service
        .port()
        .replace_before_next_close(SaveSuspensionOwned {
            discord_id: 303,
            guild_id: GUILD,
            scope: SuspensionScope::All,
            completion: SuspensionCompletion::Time,
            expires_at: Some(NOW + 600),
            matches_required: None,
            reason: "Concurrent replacement".to_owned(),
            source: ModerationSource::Admin,
            actor_id: 901,
            now: NOW + 300,
        });
    let active = service
        .get_active_suspension(303, Some(GUILD), None, NOW + 300)
        .expect("lookup")
        .expect("replacement");
    assert_eq!(active.reason, "Concurrent replacement");
    assert_eq!(active.revision, original.revision + 1);
}

#[test]
fn test_list_history_kick_and_low_priority_audit_are_guild_scoped() {
    let service = service();
    let mut low = time_request(401, GUILD, NOW + 300);
    low.scope = SuspensionScope::Lowskill;
    service.create_suspension(low).expect("suspension");
    service
        .create_suspension(time_request(402, OTHER_GUILD, NOW + 300))
        .expect("other guild");
    service
        .record_kick(RecordKick {
            discord_id: 403,
            guild_id: Some(GUILD),
            actor_id: 901,
            lobby_kind: SuspensionScope::Open,
            reason: Some("Removed"),
            source: ModerationSource::Creator,
            now: NOW,
        })
        .expect("kick");
    service
        .record_low_priority_event(RecordLowPriorityEvent {
            discord_id: 404,
            guild_id: Some(GUILD),
            event_type: ModerationEventType::LowprioAssign,
            wins_required: 3,
            wins_remaining: 3,
            actor_id: Some(902),
            reason: Some("Abandon"),
            match_id: None,
            now: NOW,
        })
        .expect("assign");
    service
        .record_low_priority_event(RecordLowPriorityEvent {
            discord_id: 404,
            guild_id: Some(GUILD),
            event_type: ModerationEventType::LowprioComplete,
            wins_required: 3,
            wins_remaining: 0,
            actor_id: None,
            reason: None,
            match_id: Some(77),
            now: NOW + 1,
        })
        .expect("complete");
    assert_eq!(
        service
            .list_active_suspensions(Some(GUILD), None, NOW)
            .expect("list")
            .iter()
            .map(|state| state.discord_id)
            .collect::<Vec<_>>(),
        vec![401]
    );
    assert!(
        service
            .list_active_suspensions(Some(GUILD), Some(SuspensionScope::Open), NOW)
            .expect("filtered")
            .is_empty()
    );
    let ids = service
        .history(Some(GUILD), None)
        .expect("history")
        .into_iter()
        .map(|event| event.discord_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(ids, BTreeSet::from([401, 403, 404]));
    let lowprio = service.history(Some(GUILD), Some(404)).expect("lowprio");
    assert_eq!(lowprio[0].event_type, ModerationEventType::LowprioComplete);
    assert_eq!(lowprio[0].wins_remaining, Some(0));
    assert_eq!(lowprio[0].match_id, Some(77));
}

#[test]
fn test_moderation_events_are_append_only() {
    let service = service();
    let event = service
        .record_kick(RecordKick {
            discord_id: 501,
            guild_id: Some(GUILD),
            actor_id: 900,
            lobby_kind: SuspensionScope::Open,
            reason: None,
            source: ModerationSource::Admin,
            now: NOW,
        })
        .expect("kick");
    let detached = service.history(Some(GUILD), Some(501)).expect("history");
    assert_eq!(detached[0], event);
    assert!(service.port().state().contract.append_only_update_trigger);
    assert!(service.port().state().contract.append_only_delete_trigger);
}
