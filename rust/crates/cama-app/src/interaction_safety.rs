//! Recovery policies for Discord interaction, follow-up, and message delivery.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    DiscordApi,
    NotFound,
    InteractionResponded,
    Local,
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

    #[must_use]
    pub fn local(type_name: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Local,
            type_name,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attachment {
    pub name: String,
    pub reset_count: usize,
}

impl Attachment {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            reset_count: 0,
        }
    }

    fn reset(&mut self) {
        self.reset_count += 1;
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SentAttachments {
    #[default]
    None,
    File(String),
    Files(Vec<String>),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SentPayload {
    pub content: Option<String>,
    pub embed: Option<String>,
    pub attachments: SentAttachments,
    pub view: bool,
    pub ephemeral: bool,
    pub allowed_mentions: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SendRequest {
    pub content: Option<String>,
    pub embed: Option<String>,
    pub attachments: Vec<Attachment>,
    pub attachments_are_files: bool,
    pub view: bool,
    pub ephemeral: bool,
    pub allowed_mentions: bool,
}

impl SendRequest {
    fn payload(&self) -> SentPayload {
        let attachments = match self.attachments.as_slice() {
            [] => SentAttachments::None,
            [attachment] if !self.attachments_are_files => {
                SentAttachments::File(attachment.name.clone())
            }
            attachments => SentAttachments::Files(
                attachments
                    .iter()
                    .map(|attachment| attachment.name.clone())
                    .collect(),
            ),
        };
        SentPayload {
            content: self.content.clone(),
            embed: self.embed.clone(),
            attachments,
            view: self.view,
            ephemeral: self.ephemeral,
            allowed_mentions: self.allowed_mentions,
        }
    }
}

pub trait DeferPort {
    fn defer(&mut self, ephemeral: bool, thinking: bool) -> Result<(), TransportError>;
}

pub trait SendPort {
    fn send(&mut self, payload: SentPayload) -> Result<String, TransportError>;
}

#[must_use]
pub fn safe_defer(port: &mut dyn DeferPort, ephemeral: bool, thinking: bool) -> bool {
    match port.defer(ephemeral, thinking) {
        Ok(()) => true,
        Err(error) if error.recoverable_discord() => false,
        Err(_) => false,
    }
}

pub trait EditableMessage {
    fn edit(&mut self, payload: SentPayload) -> Result<(), TransportError>;
    fn send_to_channel(&mut self, payload: SentPayload) -> Result<String, TransportError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditFailureScope {
    Any,
    DiscordOnly,
}

/// Edit a message and, when allowed by `scope`, resend only durable content.
/// Views are intentionally omitted from a fresh fallback message.
pub fn edit_message_with_fallback(
    message: Option<&mut dyn EditableMessage>,
    send_fallback: Option<&mut dyn SendPort>,
    request: &mut SendRequest,
    scope: EditFailureScope,
) -> Result<bool, TransportError> {
    let Some(message) = message else {
        return Ok(false);
    };
    match message.edit(request.payload()) {
        Ok(()) => return Ok(true),
        Err(error) if scope == EditFailureScope::DiscordOnly && !error.recoverable_discord() => {
            return Err(error);
        }
        Err(_) => {}
    }

    for attachment in &mut request.attachments {
        attachment.reset();
    }
    let payload = if request.embed.is_some() {
        SentPayload {
            embed: request.embed.clone(),
            attachments: if request.attachments.is_empty() {
                SentAttachments::None
            } else {
                SentAttachments::Files(
                    request
                        .attachments
                        .iter()
                        .map(|attachment| attachment.name.clone())
                        .collect(),
                )
            },
            ..SentPayload::default()
        }
    } else if request
        .content
        .as_deref()
        .is_some_and(|content| !content.is_empty())
    {
        SentPayload {
            content: request.content.clone(),
            ..SentPayload::default()
        }
    } else {
        return Ok(false);
    };

    let result = if let Some(fallback) = send_fallback {
        fallback.send(payload)
    } else {
        message.send_to_channel(payload)
    };
    Ok(result.is_ok())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosedLobbyEmbed {
    pub title: String,
    pub description: String,
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

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

    #[derive(Default)]
    struct RecordingSend {
        calls: Vec<SentPayload>,
    }

    impl SendPort for RecordingSend {
        fn send(&mut self, payload: SentPayload) -> Result<String, TransportError> {
            self.calls.push(payload);
            Ok("ok".to_owned())
        }
    }

    #[test]
    fn test_safe_defer_forwards_thinking_flag() {
        let mut defer = RecordingDefer::default();
        assert!(safe_defer(&mut defer, false, true));
        assert_eq!(defer.calls, [(false, true)]);
    }

    #[derive(Default)]
    struct RecordingMessage {
        edits: Vec<SentPayload>,
        edit_results: VecDeque<Result<(), TransportError>>,
        channel_calls: Vec<SentPayload>,
        channel_results: VecDeque<Result<String, TransportError>>,
    }

    impl EditableMessage for RecordingMessage {
        fn edit(&mut self, payload: SentPayload) -> Result<(), TransportError> {
            self.edits.push(payload);
            self.edit_results.pop_front().unwrap_or(Ok(()))
        }

        fn send_to_channel(&mut self, payload: SentPayload) -> Result<String, TransportError> {
            self.channel_calls.push(payload);
            self.channel_results
                .pop_front()
                .unwrap_or_else(|| Ok("sent".to_owned()))
        }
    }

    #[test]
    fn test_edit_message_with_fallback_edit_success_skips_fallback() {
        let mut message = RecordingMessage::default();
        let mut request = SendRequest {
            embed: Some("E".to_owned()),
            ..SendRequest::default()
        };
        assert_eq!(
            edit_message_with_fallback(
                Some(&mut message),
                None,
                &mut request,
                EditFailureScope::Any,
            ),
            Ok(true)
        );
        assert_eq!(message.edits.len(), 1);
        assert!(message.channel_calls.is_empty());
    }

    #[test]
    fn test_edit_message_with_fallback_none_message_returns_false() {
        let mut request = SendRequest {
            embed: Some("E".to_owned()),
            ..SendRequest::default()
        };
        assert_eq!(
            edit_message_with_fallback(None, None, &mut request, EditFailureScope::Any),
            Ok(false)
        );
    }

    #[test]
    fn test_edit_failure_rewinds_files_and_resends_without_view() {
        let mut message = RecordingMessage {
            edit_results: VecDeque::from([Err(TransportError::discord("edit failed"))]),
            ..RecordingMessage::default()
        };
        let mut request = SendRequest {
            embed: Some("E".to_owned()),
            attachments: vec![Attachment::new("result.png")],
            attachments_are_files: true,
            view: true,
            ..SendRequest::default()
        };
        assert_eq!(
            edit_message_with_fallback(
                Some(&mut message),
                None,
                &mut request,
                EditFailureScope::Any,
            ),
            Ok(true)
        );
        assert_eq!(request.attachments[0].reset_count, 1);
        assert_eq!(message.channel_calls.len(), 1);
        assert_eq!(message.channel_calls[0].embed.as_deref(), Some("E"));
        assert_eq!(
            message.channel_calls[0].attachments,
            SentAttachments::Files(vec!["result.png".to_owned()])
        );
        assert!(!message.channel_calls[0].view);
    }

    #[test]
    fn test_edit_failure_uses_provided_send_fallback() {
        let mut message = RecordingMessage {
            edit_results: VecDeque::from([Err(TransportError::local(
                "RuntimeError",
                "token expired",
            ))]),
            ..RecordingMessage::default()
        };
        let mut fallback = RecordingSend::default();
        let mut request = SendRequest {
            embed: Some("E".to_owned()),
            ..SendRequest::default()
        };
        assert_eq!(
            edit_message_with_fallback(
                Some(&mut message),
                Some(&mut fallback),
                &mut request,
                EditFailureScope::Any,
            ),
            Ok(true)
        );
        assert_eq!(fallback.calls.len(), 1);
        assert_eq!(fallback.calls[0].embed.as_deref(), Some("E"));
        assert!(message.channel_calls.is_empty());
    }

    #[test]
    fn test_edit_failure_content_only_resends_content() {
        let mut message = RecordingMessage {
            edit_results: VecDeque::from([Err(TransportError::discord("edit failed"))]),
            ..RecordingMessage::default()
        };
        let mut request = SendRequest {
            content: Some("hi".to_owned()),
            ..SendRequest::default()
        };
        assert_eq!(
            edit_message_with_fallback(
                Some(&mut message),
                None,
                &mut request,
                EditFailureScope::Any,
            ),
            Ok(true)
        );
        assert_eq!(message.channel_calls[0].content.as_deref(), Some("hi"));
    }

    #[test]
    fn test_edit_failure_with_nothing_resendable_returns_false() {
        let mut message = RecordingMessage {
            edit_results: VecDeque::from([Err(TransportError::discord("edit failed"))]),
            ..RecordingMessage::default()
        };
        let mut request = SendRequest {
            view: true,
            ..SendRequest::default()
        };
        assert_eq!(
            edit_message_with_fallback(
                Some(&mut message),
                None,
                &mut request,
                EditFailureScope::Any,
            ),
            Ok(false)
        );
        assert!(message.channel_calls.is_empty());
    }

    #[test]
    fn test_edit_and_fallback_both_failing_returns_false() {
        let mut message = RecordingMessage {
            edit_results: VecDeque::from([Err(TransportError::discord("edit failed"))]),
            channel_results: VecDeque::from([Err(TransportError::discord("send failed"))]),
            ..RecordingMessage::default()
        };
        let mut request = SendRequest {
            embed: Some("E".to_owned()),
            ..SendRequest::default()
        };
        assert_eq!(
            edit_message_with_fallback(
                Some(&mut message),
                None,
                &mut request,
                EditFailureScope::Any,
            ),
            Ok(false)
        );
    }

    #[test]
    fn test_edit_exceptions_scopes_the_fallback_trigger() {
        let mut message = RecordingMessage {
            edit_results: VecDeque::from([
                Err(TransportError::local("TypeError", "bad kwarg")),
                Err(TransportError::discord("edit failed")),
            ]),
            ..RecordingMessage::default()
        };
        let mut request = SendRequest {
            embed: Some("E".to_owned()),
            ..SendRequest::default()
        };
        let error = edit_message_with_fallback(
            Some(&mut message),
            None,
            &mut request,
            EditFailureScope::DiscordOnly,
        )
        .expect_err("local error must propagate");
        assert_eq!(error.type_name, "TypeError");
        assert!(message.channel_calls.is_empty());

        assert_eq!(
            edit_message_with_fallback(
                Some(&mut message),
                None,
                &mut request,
                EditFailureScope::DiscordOnly,
            ),
            Ok(true)
        );
        assert_eq!(message.channel_calls.len(), 1);
        assert_eq!(message.channel_calls[0].embed.as_deref(), Some("E"));
    }
}
