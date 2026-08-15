//! Discord-neutral `/admin moderation` command policy.
//!
//! The persistence/state machine lives in [`crate::moderation`]. This module
//! owns the command boundary: permission short-circuiting, coherent term
//! validation before deferral, exact request construction, queue eviction,
//! private notifications, and audit-friendly status text. Discord transport is
//! represented as a plan so the live Python gateway remains the sole writer.

use cama_db::moderation::{
    LobbySuspension, ModerationSource, SuspensionCompletion, SuspensionScope,
};
use thiserror::Error;

use crate::embeds::LobbyKind;
use crate::moderation::{ModerationPolicyError, parse_expiry};

pub const REASON_MIN_LENGTH: i64 = 3;
pub const REASON_MAX_LENGTH: i64 = 300;
pub const MATCHES_MIN: i64 = 1;
pub const MATCHES_MAX: i64 = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModerationCommand {
    Suspend,
    Lift,
    Status,
    List,
    History,
}

impl ModerationCommand {
    #[must_use]
    pub const fn requires_manage_guild(self) -> bool {
        match self {
            Self::Suspend | Self::Lift | Self::Status | Self::List | Self::History => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParameterContract {
    pub required: bool,
    pub min_value: Option<i64>,
    pub max_value: Option<i64>,
}

#[must_use]
pub const fn reason_parameter() -> ParameterContract {
    ParameterContract {
        required: true,
        min_value: Some(REASON_MIN_LENGTH),
        max_value: Some(REASON_MAX_LENGTH),
    }
}

#[must_use]
pub const fn matches_parameter() -> ParameterContract {
    ParameterContract {
        required: false,
        min_value: Some(MATCHES_MIN),
        max_value: Some(MATCHES_MAX),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandContext {
    pub actor_id: i64,
    pub guild_id: i64,
    pub has_admin_permission: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SuspendInput<'a> {
    pub target_id: i64,
    pub reason: &'a str,
    pub duration: Option<&'a str>,
    pub matches: Option<i64>,
    pub completion: Option<SuspensionCompletion>,
    pub scope: SuspensionScope,
    pub replace: bool,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedTerms {
    pub completion: SuspensionCompletion,
    pub expires_at: Option<i64>,
    pub matches: Option<i64>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AdminModerationError {
    #[error("Specify at least one term: duration or matches")]
    MissingTerms,
    #[error("Choose whether both terms complete on either or both")]
    AmbiguousCombinedTerms,
    #[error("Completion is only used when both duration and matches are supplied")]
    CompletionWithoutBothTerms,
    #[error("{0}")]
    Duration(ModerationPolicyError),
    #[error("matches must be between 1 and 20")]
    MatchesOutOfBounds,
    #[error("Reason must contain between 3 and 300 characters")]
    ReasonLength,
}

pub fn validate_suspend_terms(
    input: SuspendInput<'_>,
) -> Result<ValidatedTerms, AdminModerationError> {
    let reason_chars = input.reason.trim().chars().count();
    if !(usize::try_from(REASON_MIN_LENGTH).unwrap_or(3)
        ..=usize::try_from(REASON_MAX_LENGTH).unwrap_or(300))
        .contains(&reason_chars)
    {
        return Err(AdminModerationError::ReasonLength);
    }
    if input.duration.is_none() && input.matches.is_none() {
        return Err(AdminModerationError::MissingTerms);
    }
    if input.duration.is_some() && input.matches.is_some() && input.completion.is_none() {
        return Err(AdminModerationError::AmbiguousCombinedTerms);
    }
    if (input.duration.is_none() || input.matches.is_none()) && input.completion.is_some() {
        return Err(AdminModerationError::CompletionWithoutBothTerms);
    }
    if input
        .matches
        .is_some_and(|matches| !(MATCHES_MIN..=MATCHES_MAX).contains(&matches))
    {
        return Err(AdminModerationError::MatchesOutOfBounds);
    }
    let expires_at = input
        .duration
        .map(|duration| parse_expiry(duration, input.now))
        .transpose()
        .map_err(AdminModerationError::Duration)?;
    let completion = match (input.duration, input.matches, input.completion) {
        (Some(_), None, None) => SuspensionCompletion::Time,
        (None, Some(_), None) => SuspensionCompletion::Matches,
        (Some(_), Some(_), Some(completion)) => completion,
        _ => return Err(AdminModerationError::CompletionWithoutBothTerms),
    };
    Ok(ValidatedTerms {
        completion,
        expires_at,
        matches: input.matches,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistSuspension<'a> {
    pub discord_id: i64,
    pub guild_id: i64,
    pub actor_id: i64,
    pub reason: &'a str,
    pub scope: SuspensionScope,
    pub completion: SuspensionCompletion,
    pub expires_at: Option<i64>,
    pub matches: Option<i64>,
    pub source: ModerationSource,
    pub now: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveCommandOutcome {
    Saved(LobbySuspension),
    ActiveExists(LobbySuspension),
}

pub trait AdminModerationPort {
    type Error;

    fn create_suspension(
        &mut self,
        request: PersistSuspension<'_>,
    ) -> Result<SaveCommandOutcome, Self::Error>;
    fn replace_suspension(
        &mut self,
        request: PersistSuspension<'_>,
    ) -> Result<LobbySuspension, Self::Error>;
    fn active_suspension(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        kind: Option<LobbyKind>,
        now: i64,
    ) -> Result<Option<LobbySuspension>, Self::Error>;
    fn queued_lobbies(
        &mut self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<LobbyKind>, Self::Error>;
    fn eject_lobby_player(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        kind: LobbyKind,
    ) -> Result<bool, Self::Error>;
    fn lift_suspension(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        actor_id: i64,
        reason: &str,
    ) -> Result<Option<LobbySuspension>, Self::Error>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AdminCommandPlan {
    pub deferred: bool,
    pub ephemeral: bool,
    pub immediate: Option<String>,
    pub followup: Option<String>,
    pub target_dm: Option<String>,
    pub evict: Vec<LobbyKind>,
}

fn denied_plan() -> AdminCommandPlan {
    AdminCommandPlan {
        ephemeral: true,
        immediate: Some("Admin only: Manage Guild permission is required.".to_owned()),
        ..AdminCommandPlan::default()
    }
}

#[must_use]
pub fn permission_denial(
    command: ModerationCommand,
    context: CommandContext,
) -> Option<AdminCommandPlan> {
    (command.requires_manage_guild() && !context.has_admin_permission).then(denied_plan)
}

fn validation_plan(error: &AdminModerationError) -> AdminCommandPlan {
    AdminCommandPlan {
        ephemeral: true,
        immediate: Some(error.to_string()),
        ..AdminCommandPlan::default()
    }
}

pub fn suspend<P: AdminModerationPort>(
    port: &mut P,
    context: CommandContext,
    input: SuspendInput<'_>,
) -> Result<AdminCommandPlan, P::Error> {
    if !context.has_admin_permission {
        return Ok(denied_plan());
    }
    let terms = match validate_suspend_terms(input) {
        Ok(terms) => terms,
        Err(error) => return Ok(validation_plan(&error)),
    };
    let request = PersistSuspension {
        discord_id: input.target_id,
        guild_id: context.guild_id,
        actor_id: context.actor_id,
        reason: input.reason.trim(),
        scope: input.scope,
        completion: terms.completion,
        expires_at: terms.expires_at,
        matches: terms.matches,
        source: ModerationSource::Admin,
        now: input.now,
    };
    let (state, replaced) = if input.replace {
        (port.replace_suspension(request)?, true)
    } else {
        match port.create_suspension(request)? {
            SaveCommandOutcome::Saved(state) => (state, false),
            SaveCommandOutcome::ActiveExists(existing) => {
                return Ok(AdminCommandPlan {
                    deferred: true,
                    ephemeral: true,
                    followup: Some(format!(
                        "<@{}> already has an active suspension: {} Reason: {}. Pass replace: True to replace it.",
                        input.target_id,
                        format_terms(&existing),
                        existing.reason
                    )),
                    ..AdminCommandPlan::default()
                });
            }
        }
    };
    let queued = port.queued_lobbies(input.target_id, context.guild_id)?;
    let mut evicted = Vec::new();
    for kind in queued {
        if port
            .active_suspension(input.target_id, context.guild_id, Some(kind), input.now)?
            .is_some()
            && port.eject_lobby_player(input.target_id, context.guild_id, kind)?
        {
            evicted.push(kind);
        }
    }
    let removed = (!evicted.is_empty()).then(|| {
        format!(
            " Removed from {}.",
            evicted
                .iter()
                .map(|kind| kind.label())
                .collect::<Vec<_>>()
                .join(" and ")
        )
    });
    let verb = if replaced {
        "Replaced the active suspension for"
    } else {
        "Suspended"
    };
    Ok(AdminCommandPlan {
        deferred: true,
        ephemeral: true,
        followup: Some(format!(
            "{verb} <@{}>. {}{}",
            input.target_id,
            format_terms(&state),
            removed.unwrap_or_default()
        )),
        target_dm: Some(format!(
            "You were suspended from {}. {} Reason: {}",
            scope_label(state.scope),
            format_terms(&state),
            state.reason
        )),
        evict: evicted,
        ..AdminCommandPlan::default()
    })
}

pub fn lift<P: AdminModerationPort>(
    port: &mut P,
    context: CommandContext,
    target_id: i64,
    reason: &str,
) -> Result<AdminCommandPlan, P::Error> {
    if !context.has_admin_permission {
        return Ok(denied_plan());
    }
    let state =
        port.lift_suspension(target_id, context.guild_id, context.actor_id, reason.trim())?;
    let Some(_state) = state else {
        return Ok(AdminCommandPlan {
            ephemeral: true,
            immediate: Some(format!("<@{target_id}> has no active suspension.")),
            ..AdminCommandPlan::default()
        });
    };
    Ok(AdminCommandPlan {
        ephemeral: true,
        immediate: Some(format!(
            "Lifted <@{target_id}>'s suspension. Reason: {}",
            reason.trim()
        )),
        target_dm: Some(format!(
            "Your lobby suspension was lifted. Reason: {}",
            reason.trim()
        )),
        ..AdminCommandPlan::default()
    })
}

pub fn status<P: AdminModerationPort>(
    port: &mut P,
    context: CommandContext,
    target_id: i64,
    now: i64,
) -> Result<AdminCommandPlan, P::Error> {
    if !context.has_admin_permission {
        return Ok(denied_plan());
    }
    let state = port.active_suspension(target_id, context.guild_id, None, now)?;
    let content = state.map_or_else(
        || format!("<@{target_id}> has no active suspension."),
        |state| {
            format!(
                "<@{target_id}> — {} — {}\nReason: {}\nActor: <@{}>\nCreated: <t:{}:F>",
                scope_label(state.scope),
                format_terms(&state),
                state.reason,
                state.actor_id,
                state.created_at
            )
        },
    );
    Ok(AdminCommandPlan {
        ephemeral: true,
        immediate: Some(content),
        ..AdminCommandPlan::default()
    })
}

#[must_use]
pub fn scope_label(scope: SuspensionScope) -> &'static str {
    match scope {
        SuspensionScope::All => "all lobbies",
        SuspensionScope::Open => "All You Can Feed",
        SuspensionScope::Lowskill => "Whine & Cheese",
    }
}

#[must_use]
pub fn format_terms(state: &LobbySuspension) -> String {
    let time = state.expires_at.map(|expires| format!("<t:{expires}:F>"));
    let matches = state
        .matches_remaining
        .map(|remaining| format!("{remaining} completed matches remaining"));
    match state.completion {
        SuspensionCompletion::Time => time.unwrap_or_else(|| "time term missing".to_owned()),
        SuspensionCompletion::Matches => matches.unwrap_or_else(|| "match term missing".to_owned()),
        SuspensionCompletion::Either => format!(
            "{} or {}",
            time.unwrap_or_else(|| "time term missing".to_owned()),
            matches.unwrap_or_else(|| "match term missing".to_owned())
        ),
        SuspensionCompletion::Both => format!(
            "{} and {}",
            time.unwrap_or_else(|| "time term missing".to_owned()),
            matches.unwrap_or_else(|| "match term missing".to_owned())
        ),
    }
}

#[cfg(test)]
mod tests;
