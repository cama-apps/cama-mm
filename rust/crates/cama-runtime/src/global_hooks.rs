//! Transport-neutral global Discord interaction hooks.
//!
//! This is the Rust equivalent of Python's `bot.tree.error` and
//! `on_interaction`: one process-wide usage monitor, exact command naming,
//! typed transformer handling, and a survey path that never retains or emits
//! the sensitive raw option value.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracing::error;

use crate::registration::{
    InteractionHandlerError, InteractionRequest, InteractionResponder, InteractionResponse,
};

const GENERIC_ERROR: &str = "An error occurred while processing your command. Please try again.";
const SURVEY_TRANSFORM_ERROR: &str = "Could not resolve that survey target. Please use @mention or select from Discord's picker when typing.";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UsageSnapshot {
    pub command_total: u64,
    pub command_failures: u64,
    pub commands_by_name: BTreeMap<String, u64>,
    /// Popularity order retained for Python-compatible health presentation.
    pub commands_ranked: Vec<(String, u64)>,
    pub api_requests: BTreeMap<String, u64>,
}

/// Thread-safe, since-startup counters shared by the gateway and health
/// surfaces. The snapshot retains only the eight most-used commands, matching
/// Python's `Counter.most_common(8)` policy, including first-seen tie order.
#[derive(Clone, Debug, Default)]
pub struct UsageMonitor {
    inner: Arc<Mutex<UsageState>>,
}

#[derive(Debug, Default)]
struct UsageState {
    command_total: u64,
    command_failures: u64,
    commands_by_name: BTreeMap<String, u64>,
    command_first_seen: Vec<String>,
    api_requests: BTreeMap<String, u64>,
}

impl UsageMonitor {
    pub fn record_command(&self, name: Option<&str>) {
        let name = name.filter(|name| !name.is_empty()).unwrap_or("unknown");
        let mut usage = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        usage.command_total = usage.command_total.saturating_add(1);
        if !usage.commands_by_name.contains_key(name) {
            usage.command_first_seen.push(name.to_owned());
        }
        let count = usage.commands_by_name.entry(name.to_owned()).or_default();
        *count = count.saturating_add(1);
    }

    pub fn record_command_failure(&self) {
        let mut usage = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        usage.command_failures = usage.command_failures.saturating_add(1);
    }

    pub fn record_api_request(&self, provider: &str) {
        let mut usage = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let count = usage.api_requests.entry(provider.to_owned()).or_default();
        *count = count.saturating_add(1);
    }

    #[must_use]
    pub fn snapshot(&self) -> UsageSnapshot {
        let usage = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let mut commands = usage
            .command_first_seen
            .iter()
            .enumerate()
            .map(|(order, name)| {
                (
                    name.clone(),
                    usage
                        .commands_by_name
                        .get(name)
                        .copied()
                        .unwrap_or_default(),
                    order,
                )
            })
            .collect::<Vec<_>>();
        commands.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.2.cmp(&right.2)));
        let commands_ranked = commands
            .iter()
            .take(8)
            .map(|(name, count, _)| (name.clone(), *count))
            .collect::<Vec<_>>();
        UsageSnapshot {
            command_total: usage.command_total,
            command_failures: usage.command_failures,
            commands_by_name: commands
                .into_iter()
                .take(8)
                .map(|(name, count, _)| (name, count))
                .collect(),
            commands_ranked,
            api_requests: usage.api_requests.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GlobalInteractionHooks {
    usage: UsageMonitor,
}

impl GlobalInteractionHooks {
    #[must_use]
    pub fn new(usage: UsageMonitor) -> Self {
        Self { usage }
    }

    #[must_use]
    pub const fn usage_monitor(&self) -> &UsageMonitor {
        &self.usage
    }

    /// Observe an application command before route lookup/handler execution.
    /// Components never reach this method in production.
    pub fn observe_command(&self, qualified_name: Option<&str>) {
        self.usage.record_command(qualified_name);
    }

    /// Apply Python's global tree-error policy. Error delivery itself is
    /// fail-soft: a Discord response failure is logged and not propagated into
    /// the gateway event loop.
    pub async fn handle_error(
        &self,
        request: &InteractionRequest,
        qualified_name: Option<&str>,
        handler_error: &InteractionHandlerError,
        responder: Arc<dyn InteractionResponder>,
    ) {
        self.usage.record_command_failure();
        let root_name = request.root_name();
        let is_survey = is_survey_command(root_name, qualified_name);
        let audit = interaction_error_audit(root_name, qualified_name, handler_error, is_survey);

        if let Some(detail) = audit.detail {
            error!(
                command = audit.command,
                error_category = audit.category,
                error = detail,
                "app command error"
            );
        } else {
            error!(
                command = audit.command,
                error_category = audit.category,
                "app command error in survey command"
            );
        }

        let message = match (handler_error, is_survey) {
            (InteractionHandlerError::Transformer { .. }, true) => {
                SURVEY_TRANSFORM_ERROR.to_owned()
            }
            (InteractionHandlerError::Transformer { value }, false) => format!(
                "Could not find user `{value}`. Please use @mention or select from Discord's user picker when typing."
            ),
            (InteractionHandlerError::Handler(_), _) => GENERIC_ERROR.to_owned(),
        };
        let mut response = InteractionResponse::message(format!("❌ {message}")).ephemeral();
        if is_survey {
            response = response.without_mentions();
        }
        let result = if responder.response_is_done() {
            responder.followup(response).await
        } else {
            responder.respond(response).await
        };
        if let Err(response_error) = result {
            error!(%response_error, "failed to send app-command error message to user");
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct InteractionErrorAudit {
    command: String,
    category: &'static str,
    detail: Option<String>,
}

fn interaction_error_audit(
    root_name: &str,
    qualified_name: Option<&str>,
    handler_error: &InteractionHandlerError,
    is_survey: bool,
) -> InteractionErrorAudit {
    InteractionErrorAudit {
        command: if is_survey {
            "survey".to_owned()
        } else {
            qualified_name
                .and_then(|name| name.split_whitespace().last())
                .filter(|name| !name.is_empty())
                .unwrap_or(root_name)
                .to_owned()
        },
        category: handler_error.category(),
        detail: (!is_survey).then(|| handler_error.to_string()),
    }
}

fn is_survey_command(root_name: &str, qualified_name: Option<&str>) -> bool {
    root_name == "survey"
        || qualified_name.is_some_and(|name| name == "survey" || name.starts_with("survey "))
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests;
