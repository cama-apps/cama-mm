use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use super::*;
use crate::registration::{InteractionAllowedMentions, InteractionResponseError};

#[derive(Default)]
struct CapturingResponder {
    done: AtomicBool,
    initial: Mutex<Vec<InteractionResponse>>,
    followups: Mutex<Vec<InteractionResponse>>,
    fail_delivery: AtomicBool,
}

impl CapturingResponder {
    fn deferred() -> Self {
        Self {
            done: AtomicBool::new(true),
            ..Self::default()
        }
    }
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        if self.fail_delivery.load(Ordering::Relaxed) {
            return Err(InteractionResponseError::new("offline response failure"));
        }
        self.initial
            .lock()
            .expect("initial responses")
            .push(response);
        self.done.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.done.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        if self.fail_delivery.load(Ordering::Relaxed) {
            return Err(InteractionResponseError::new("offline followup failure"));
        }
        self.followups
            .lock()
            .expect("followup responses")
            .push(response);
        Ok(())
    }

    fn response_is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

fn command(name: &str) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: name.to_owned(),
        user_id: 2,
        user_display_name: "User".to_owned(),
        guild_id: Some(3),
        channel_id: Some(4),
        member_permissions: None,
        options: Vec::new(),
    }
}

#[test]
fn usage_monitor_matches_python_names_failures_and_top_eight_snapshot() {
    let monitor = UsageMonitor::default();
    let hooks = GlobalInteractionHooks::new(monitor.clone());
    hooks.observe_command(Some("admin health"));
    hooks.observe_command(Some("admin health"));
    hooks.observe_command(Some("lobby"));
    for name in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        hooks.observe_command(Some(name));
    }
    monitor.record_command_failure();
    monitor.record_api_request("opendota");
    monitor.record_api_request("opendota");
    monitor.record_api_request("valve");

    let snapshot = monitor.snapshot();
    assert_eq!(snapshot.command_total, 11);
    assert_eq!(snapshot.command_failures, 1);
    assert_eq!(snapshot.commands_by_name.len(), 8);
    assert_eq!(snapshot.commands_by_name["admin health"], 2);
    assert_eq!(snapshot.commands_by_name["lobby"], 1);
    assert_eq!(
        snapshot.api_requests,
        BTreeMap::from([("opendota".to_owned(), 2), ("valve".to_owned(), 1),])
    );

    let unknown = UsageMonitor::default();
    GlobalInteractionHooks::new(unknown.clone()).observe_command(None);
    assert_eq!(unknown.snapshot().commands_by_name["unknown"], 1);
}

#[tokio::test]
async fn survey_transformer_error_redacts_value_disables_mentions_and_uses_initial_response() {
    let hooks = GlobalInteractionHooks::default();
    let responder = Arc::new(CapturingResponder::default());
    let sensitive = "123456789012345678";
    hooks
        .handle_error(
            &command("survey"),
            Some("survey send"),
            &InteractionHandlerError::Transformer {
                value: sensitive.to_owned(),
            },
            responder.clone(),
        )
        .await;

    let responses = responder.initial.lock().expect("initial responses");
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0].content,
        "❌ Could not resolve that survey target. Please use @mention or select from Discord's picker when typing."
    );
    assert!(responses[0].ephemeral);
    assert_eq!(
        responses[0].allowed_mentions,
        InteractionAllowedMentions::None
    );
    assert!(!responses[0].content.contains(sensitive));
    assert_eq!(hooks.usage_monitor().snapshot().command_failures, 1);
    let audit = interaction_error_audit(
        "survey",
        Some("survey send"),
        &InteractionHandlerError::Transformer {
            value: sensitive.to_owned(),
        },
        true,
    );
    let rendered_audit = format!("{audit:?}");
    assert_eq!(audit.command, "survey");
    assert_eq!(audit.category, "TransformerError");
    assert_eq!(audit.detail, None);
    assert!(!rendered_audit.contains(sensitive));
}

#[tokio::test]
async fn ordinary_transformer_error_preserves_python_value_and_uses_followup_after_defer() {
    let hooks = GlobalInteractionHooks::default();
    let responder = Arc::new(CapturingResponder::deferred());
    hooks
        .handle_error(
            &command("player"),
            Some("player register"),
            &InteractionHandlerError::Transformer {
                value: "typed user".to_owned(),
            },
            responder.clone(),
        )
        .await;

    assert!(
        responder
            .initial
            .lock()
            .expect("initial responses")
            .is_empty()
    );
    assert_eq!(
        responder.followups.lock().expect("followup responses")[0],
        InteractionResponse::message(
            "❌ Could not find user `typed user`. Please use @mention or select from Discord's user picker when typing."
        )
        .ephemeral()
    );
}

#[tokio::test]
async fn generic_and_response_delivery_failures_are_fail_soft() {
    let hooks = GlobalInteractionHooks::default();
    let responder = Arc::new(CapturingResponder::default());
    hooks
        .handle_error(
            &command("profile"),
            Some("profile"),
            &InteractionHandlerError::Handler("database secret detail".to_owned()),
            responder.clone(),
        )
        .await;
    assert_eq!(
        responder.initial.lock().expect("initial responses")[0].content,
        "❌ An error occurred while processing your command. Please try again."
    );

    let failing = Arc::new(CapturingResponder::default());
    failing.fail_delivery.store(true, Ordering::Relaxed);
    hooks
        .handle_error(
            &command("profile"),
            Some("profile"),
            &InteractionHandlerError::Handler("another error".to_owned()),
            failing,
        )
        .await;
    assert_eq!(hooks.usage_monitor().snapshot().command_failures, 2);
}
