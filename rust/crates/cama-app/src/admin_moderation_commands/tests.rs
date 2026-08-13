use super::*;

const ADMIN_ID: i64 = 900;
const TARGET_ID: i64 = 42;
const GUILD_ID: i64 = 77;

#[derive(Clone, Debug, Eq, PartialEq)]
struct OwnedPersist {
    discord_id: i64,
    guild_id: i64,
    actor_id: i64,
    reason: String,
    scope: SuspensionScope,
    completion: SuspensionCompletion,
    expires_at: Option<i64>,
    matches: Option<i64>,
    source: ModerationSource,
    now: i64,
}

impl From<PersistSuspension<'_>> for OwnedPersist {
    fn from(request: PersistSuspension<'_>) -> Self {
        Self {
            discord_id: request.discord_id,
            guild_id: request.guild_id,
            actor_id: request.actor_id,
            reason: request.reason.to_owned(),
            scope: request.scope,
            completion: request.completion,
            expires_at: request.expires_at,
            matches: request.matches,
            source: request.source,
            now: request.now,
        }
    }
}

fn state() -> LobbySuspension {
    LobbySuspension {
        discord_id: TARGET_ID,
        guild_id: GUILD_ID,
        scope: SuspensionScope::All,
        completion: SuspensionCompletion::Time,
        reason: "repeat abandonment".to_owned(),
        source: ModerationSource::Admin,
        actor_id: ADMIN_ID,
        expires_at: Some(8_200),
        matches_required: None,
        matches_remaining: None,
        pending_match_watermark: 0,
        revision: 1,
        active: true,
        created_at: 1_000,
        updated_at: 1_000,
    }
}

#[derive(Default)]
struct FakePort {
    calls: Vec<String>,
    create_requests: Vec<OwnedPersist>,
    replace_requests: Vec<OwnedPersist>,
    lift_requests: Vec<(i64, i64, i64, String)>,
    create_outcome: Option<SaveCommandOutcome>,
    replacement: Option<LobbySuspension>,
    lifted: Option<LobbySuspension>,
    queued: Vec<LobbyKind>,
    active_general: Option<LobbySuspension>,
    active_open: Option<LobbySuspension>,
    active_low: Option<LobbySuspension>,
    ejected: Vec<LobbyKind>,
}

impl AdminModerationPort for FakePort {
    type Error = String;

    fn create_suspension(
        &mut self,
        request: PersistSuspension<'_>,
    ) -> Result<SaveCommandOutcome, Self::Error> {
        self.calls.push("create".to_owned());
        self.create_requests.push(request.into());
        Ok(self
            .create_outcome
            .clone()
            .unwrap_or_else(|| SaveCommandOutcome::Saved(state())))
    }

    fn replace_suspension(
        &mut self,
        request: PersistSuspension<'_>,
    ) -> Result<LobbySuspension, Self::Error> {
        self.calls.push("replace".to_owned());
        self.replace_requests.push(request.into());
        Ok(self.replacement.clone().unwrap_or_else(state))
    }

    fn active_suspension(
        &mut self,
        _discord_id: i64,
        _guild_id: i64,
        kind: Option<LobbyKind>,
        _now: i64,
    ) -> Result<Option<LobbySuspension>, Self::Error> {
        self.calls.push("active".to_owned());
        Ok(match kind {
            None => self.active_general.clone(),
            Some(LobbyKind::Open) => self.active_open.clone(),
            Some(LobbyKind::LowSkill) => self.active_low.clone(),
        })
    }

    fn queued_lobbies(
        &mut self,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<Vec<LobbyKind>, Self::Error> {
        self.calls.push("queued".to_owned());
        Ok(self.queued.clone())
    }

    fn eject_lobby_player(
        &mut self,
        _discord_id: i64,
        _guild_id: i64,
        kind: LobbyKind,
    ) -> Result<bool, Self::Error> {
        self.calls.push("eject".to_owned());
        self.ejected.push(kind);
        Ok(true)
    }

    fn lift_suspension(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        actor_id: i64,
        reason: &str,
    ) -> Result<Option<LobbySuspension>, Self::Error> {
        self.calls.push("lift".to_owned());
        self.lift_requests
            .push((discord_id, guild_id, actor_id, reason.to_owned()));
        Ok(self.lifted.clone())
    }
}

fn context(authorized: bool) -> CommandContext {
    CommandContext {
        actor_id: ADMIN_ID,
        guild_id: GUILD_ID,
        has_admin_permission: authorized,
    }
}

fn suspend_input() -> SuspendInput<'static> {
    SuspendInput {
        target_id: TARGET_ID,
        reason: "repeat abandonment",
        duration: Some("2h"),
        matches: None,
        completion: None,
        scope: SuspensionScope::All,
        replace: false,
        now: 1_000,
    }
}

fn assert_permission_denied(command: ModerationCommand) {
    let plan = permission_denial(command, context(false)).expect("denied plan");
    assert!(plan.ephemeral);
    assert!(plan.immediate.expect("message").contains("Admin only"));
}

#[test]
fn test_moderation_commands_require_manage_guild_by_default() {
    for command in [
        ModerationCommand::Suspend,
        ModerationCommand::Lift,
        ModerationCommand::Status,
        ModerationCommand::List,
        ModerationCommand::History,
    ] {
        assert!(command.requires_manage_guild());
    }
}

#[test]
fn test_suspend_exposes_bounded_terms_and_required_reason() {
    assert_eq!(
        reason_parameter(),
        ParameterContract {
            required: true,
            min_value: Some(3),
            max_value: Some(300),
        }
    );
    assert_eq!(matches_parameter().min_value, Some(1));
    assert_eq!(matches_parameter().max_value, Some(20));
}

#[test]
fn test_moderation_runtime_permission_blocks_service_access_moderation_suspend_true_kwargs0() {
    let mut port = FakePort::default();
    let plan = suspend(&mut port, context(false), suspend_input()).expect("denied");
    assert!(plan.immediate.expect("message").contains("Admin only"));
    assert!(port.calls.is_empty());
}

#[test]
fn test_moderation_runtime_permission_blocks_service_access_moderation_lift_true_kwargs1() {
    let mut port = FakePort::default();
    let plan = lift(&mut port, context(false), TARGET_ID, "appeal accepted").expect("denied");
    assert!(plan.immediate.expect("message").contains("Admin only"));
    assert!(port.calls.is_empty());
}

#[test]
fn test_moderation_runtime_permission_blocks_service_access_moderation_status_true_kwargs2() {
    let mut port = FakePort::default();
    let plan = status(&mut port, context(false), TARGET_ID, 1_000).expect("denied");
    assert!(plan.immediate.expect("message").contains("Admin only"));
    assert!(port.calls.is_empty());
}

#[test]
fn test_moderation_runtime_permission_blocks_service_access_moderation_list_false_kwargs3() {
    assert_permission_denied(ModerationCommand::List);
}

#[test]
fn test_moderation_runtime_permission_blocks_service_access_moderation_history_false_kwargs4() {
    assert_permission_denied(ModerationCommand::History);
}

fn assert_rejected(mut input: SuspendInput<'static>, expected: &str) {
    let mut port = FakePort::default();
    input.now = 1_000;
    let plan = suspend(&mut port, context(true), input).expect("validation plan");
    assert!(!plan.deferred);
    assert!(plan.immediate.expect("message").contains(expected));
    assert!(port.calls.is_empty());
}

#[test]
fn test_suspend_rejects_incoherent_terms_before_deferring_kwargs0_at_least_one_term() {
    assert_rejected(
        SuspendInput {
            duration: None,
            ..suspend_input()
        },
        "at least one term",
    );
}

#[test]
fn test_suspend_rejects_incoherent_terms_before_deferring_kwargs1_choose_whether() {
    assert_rejected(
        SuspendInput {
            matches: Some(3),
            ..suspend_input()
        },
        "Choose whether",
    );
}

#[test]
fn test_suspend_rejects_incoherent_terms_before_deferring_kwargs2_only_used_when_both() {
    assert_rejected(
        SuspendInput {
            completion: Some(SuspensionCompletion::Either),
            ..suspend_input()
        },
        "only used when both",
    );
}

#[test]
fn test_suspend_surfaces_duration_validation_privately() {
    assert_rejected(
        SuspendInput {
            duration: Some("1m"),
            ..suspend_input()
        },
        "5 minutes and 365 days",
    );
}

#[test]
fn test_suspend_creates_exact_time_scoped_state_and_notifies_privately() {
    let mut port = FakePort::default();
    let plan = suspend(&mut port, context(true), suspend_input()).expect("suspend");
    assert_eq!(port.create_requests.len(), 1);
    assert_eq!(
        port.create_requests[0],
        OwnedPersist {
            discord_id: TARGET_ID,
            guild_id: GUILD_ID,
            actor_id: ADMIN_ID,
            reason: "repeat abandonment".to_owned(),
            scope: SuspensionScope::All,
            completion: SuspensionCompletion::Time,
            expires_at: Some(8_200),
            matches: None,
            source: ModerationSource::Admin,
            now: 1_000,
        }
    );
    assert!(plan.deferred && plan.ephemeral);
    assert!(plan.target_dm.expect("dm").contains("repeat abandonment"));
    assert!(plan.followup.expect("followup").contains("Suspended <@42>"));
}

#[test]
fn test_all_scope_suspension_evicts_every_queued_lobby() {
    let active = state();
    let mut port = FakePort {
        queued: vec![LobbyKind::Open, LobbyKind::LowSkill],
        active_open: Some(active.clone()),
        active_low: Some(active),
        ..FakePort::default()
    };
    let plan = suspend(&mut port, context(true), suspend_input()).expect("suspend");
    assert_eq!(port.ejected, [LobbyKind::Open, LobbyKind::LowSkill]);
    assert_eq!(plan.evict, [LobbyKind::Open, LobbyKind::LowSkill]);
    let followup = plan.followup.expect("followup");
    assert!(followup.contains(LobbyKind::Open.label()));
    assert!(followup.contains(LobbyKind::LowSkill.label()));
    assert!(!followup.contains("already-starting match"));
}

#[test]
fn test_lowskill_scoped_suspension_leaves_the_open_seat_intact() {
    let low_state = LobbySuspension {
        scope: SuspensionScope::Lowskill,
        ..state()
    };
    let mut port = FakePort {
        create_outcome: Some(SaveCommandOutcome::Saved(low_state.clone())),
        queued: vec![LobbyKind::Open, LobbyKind::LowSkill],
        active_low: Some(low_state),
        ..FakePort::default()
    };
    let plan = suspend(
        &mut port,
        context(true),
        SuspendInput {
            scope: SuspensionScope::Lowskill,
            ..suspend_input()
        },
    )
    .expect("suspend");
    assert_eq!(port.ejected, [LobbyKind::LowSkill]);
    assert_eq!(plan.evict, [LobbyKind::LowSkill]);
    let removed = plan.followup.expect("followup");
    assert!(removed.contains(LobbyKind::LowSkill.label()));
    assert!(
        !removed
            .split("Removed from")
            .last()
            .unwrap_or_default()
            .contains(LobbyKind::Open.label())
    );
}

#[test]
fn test_suspend_without_replace_flag_reports_existing_suspension() {
    let existing = state();
    let mut port = FakePort {
        create_outcome: Some(SaveCommandOutcome::ActiveExists(existing)),
        ..FakePort::default()
    };
    let plan = suspend(
        &mut port,
        context(true),
        SuspendInput {
            duration: None,
            matches: Some(5),
            scope: SuspensionScope::Open,
            ..suspend_input()
        },
    )
    .expect("existing");
    assert_eq!(port.create_requests.len(), 1);
    assert!(port.replace_requests.is_empty());
    assert!(plan.target_dm.is_none());
    let content = plan.followup.expect("followup");
    assert!(content.contains("already has an active suspension"));
    assert!(content.contains("<t:8200:F>"));
    assert!(content.contains("repeat abandonment"));
    assert!(content.contains("replace: True"));
}

#[test]
fn test_suspend_replaces_active_state_only_with_explicit_flag() {
    let replacement = LobbySuspension {
        scope: SuspensionScope::Open,
        completion: SuspensionCompletion::Matches,
        expires_at: None,
        matches_required: Some(5),
        matches_remaining: Some(5),
        ..state()
    };
    let mut port = FakePort {
        replacement: Some(replacement),
        ..FakePort::default()
    };
    let plan = suspend(
        &mut port,
        context(true),
        SuspendInput {
            reason: "new exact term",
            duration: None,
            matches: Some(5),
            scope: SuspensionScope::Open,
            replace: true,
            now: 2_000,
            ..suspend_input()
        },
    )
    .expect("replace");
    assert_eq!(port.replace_requests.len(), 1);
    assert!(port.create_requests.is_empty());
    assert_eq!(port.replace_requests[0].reason, "new exact term");
    let content = plan.followup.expect("followup");
    assert!(content.contains("Replaced the active suspension"));
    assert!(content.contains("5 completed matches remaining"));
}

#[test]
fn test_moderation_lift_records_actor_reason_and_dms_target() {
    let mut port = FakePort {
        lifted: Some(state()),
        ..FakePort::default()
    };
    let plan = lift(&mut port, context(true), TARGET_ID, "appeal accepted").expect("lift");
    assert_eq!(
        port.lift_requests,
        [(TARGET_ID, GUILD_ID, ADMIN_ID, "appeal accepted".to_owned())]
    );
    assert!(plan.target_dm.expect("dm").contains("appeal accepted"));
    let message = plan.immediate.expect("message");
    assert!(message.contains("Lifted <@42>"));
    assert!(message.contains("appeal accepted"));
    assert!(plan.ephemeral);
}

#[test]
fn test_moderation_status_shows_private_scope_terms_and_audit_fields() {
    let current = LobbySuspension {
        scope: SuspensionScope::Lowskill,
        completion: SuspensionCompletion::Both,
        expires_at: Some(9_000),
        matches_required: Some(4),
        matches_remaining: Some(2),
        ..state()
    };
    let mut port = FakePort {
        active_general: Some(current),
        ..FakePort::default()
    };
    let plan = status(&mut port, context(true), TARGET_ID, 2_000).expect("status");
    let message = plan.immediate.expect("message");
    for expected in [
        "Whine & Cheese",
        "<t:9000:F>",
        "and 2 completed matches remaining",
        "repeat abandonment",
        "<@900>",
        "<t:1000:F>",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
    assert!(plan.ephemeral);
}
