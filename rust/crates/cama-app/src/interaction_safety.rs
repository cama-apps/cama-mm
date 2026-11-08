//! Recovery policies for Discord interaction, follow-up, and message delivery.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    DiscordApi,
    NotFound,
    InteractionResponded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub type_name: &'static str,
    pub message: String,
}

impl TransportError {
    #[must_use]
    pub fn discord(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::DiscordApi,
            type_name: "HTTPException",
            message: message.into(),
        }
    }

    const fn recoverable_discord(&self) -> bool {
        matches!(
            self.kind,
            TransportErrorKind::DiscordApi
                | TransportErrorKind::NotFound
                | TransportErrorKind::InteractionResponded
        )
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

pub trait DeferPort {
    fn defer(&mut self, ephemeral: bool, thinking: bool) -> Result<(), TransportError>;
}

#[must_use]
pub fn safe_defer(port: &mut dyn DeferPort, ephemeral: bool, thinking: bool) -> bool {
    match port.defer(ephemeral, thinking) {
        Ok(()) => true,
        Err(error) if error.recoverable_discord() => false,
        Err(_) => false,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosedLobbyEmbed {
    pub title: String,
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingDefer {
        calls: Vec<(bool, bool)>,
        result: Option<Result<(), TransportError>>,
    }

    impl DeferPort for RecordingDefer {
        fn defer(&mut self, ephemeral: bool, thinking: bool) -> Result<(), TransportError> {
            self.calls.push((ephemeral, thinking));
            self.result.take().unwrap_or(Ok(()))
        }
    }

    #[test]
    fn test_safe_defer_forwards_thinking_flag() {
        let mut defer = RecordingDefer::default();
        assert!(safe_defer(&mut defer, false, true));
        assert_eq!(defer.calls, [(false, true)]);
    }
}
