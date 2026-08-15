//! Recovery policies for Discord interaction, follow-up, and message delivery.

use std::fmt;

use crate::embeds::LobbyKind;

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

    fn without_attachments_ephemeral(&self) -> Self {
        Self {
            content: self.content.clone(),
            embed: self.embed.clone(),
            view: self.view,
            ephemeral: true,
            allowed_mentions: self.allowed_mentions,
            ..Self::default()
        }
    }
}

pub trait DeferPort {
    fn defer(&mut self, ephemeral: bool, thinking: bool) -> Result<(), TransportError>;
}

pub trait SendPort {
    fn send(&mut self, payload: SentPayload) -> Result<String, TransportError>;
}

pub struct InteractionPorts<'a> {
    pub followup: &'a mut dyn SendPort,
    pub channel: Option<&'a mut dyn SendPort>,
}

#[must_use]
pub fn safe_defer(port: &mut dyn DeferPort, ephemeral: bool, thinking: bool) -> bool {
    match port.defer(ephemeral, thinking) {
        Ok(()) => true,
        Err(error) if error.recoverable_discord() => false,
        Err(_) => false,
    }
}

/// Send a follow-up, recovering only public Discord API failures through the
/// interaction's channel. Ephemeral content can never cross that boundary.
pub fn safe_followup(
    ports: &mut InteractionPorts<'_>,
    request: &mut SendRequest,
) -> Result<String, TransportError> {
    match ports.followup.send(request.payload()) {
        Ok(message) => Ok(message),
        Err(error) if error.recoverable_discord() => {
            if request.ephemeral {
                return Err(error);
            }
            let Some(channel) = ports.channel.as_deref_mut() else {
                return Err(error);
            };
            for attachment in &mut request.attachments {
                attachment.reset();
            }
            channel.send(request.payload())
        }
        Err(error) => Err(error),
    }
}

/// Public-first delivery with a private attachment-free retry and a final
/// diagnostic note that identifies the original transport error.
pub fn send_public_or_ephemeral(
    ports: &mut InteractionPorts<'_>,
    request: &mut SendRequest,
) -> Option<String> {
    let original_error = match safe_followup(ports, request) {
        Ok(message) => return Some(message),
        Err(error) => error,
    };

    let mut private_retry = request.without_attachments_ephemeral();
    if let Ok(message) = safe_followup(ports, &mut private_retry) {
        return Some(message);
    }

    ports
        .followup
        .send(SentPayload {
            content: Some(format!(
                "⚠️ Couldn't display this here ({}: {}).",
                original_error.type_name, original_error
            )),
            ephemeral: true,
            ..SentPayload::default()
        })
        .ok()
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

pub trait LobbyClosePort {
    fn lobby_message_id(&self, guild_id: Option<i64>, kind: LobbyKind) -> Option<i64>;
    fn lobby_channel_id(&self, guild_id: Option<i64>, kind: LobbyKind) -> Option<i64>;
    fn has_cached_channel(&self, channel_id: i64) -> bool;
    fn fetch_channel(&mut self, channel_id: i64) -> Result<(), TransportError>;
    fn edit_partial_message(
        &mut self,
        channel_id: i64,
        message_id: i64,
        embed: ClosedLobbyEmbed,
        clear_view: bool,
    ) -> Result<(), TransportError>;
}

/// Close the persisted lobby message through a partial-message PATCH. Failures
/// are deliberately fail-open because this is presentation cleanup.
pub fn update_lobby_message_closed(
    port: &mut dyn LobbyClosePort,
    reason: &str,
    guild_id: Option<i64>,
    kind: LobbyKind,
) {
    let (Some(message_id), Some(channel_id)) = (
        port.lobby_message_id(guild_id, kind),
        port.lobby_channel_id(guild_id, kind),
    ) else {
        return;
    };
    if !port.has_cached_channel(channel_id) && port.fetch_channel(channel_id).is_err() {
        return;
    }
    let _ = port.edit_partial_message(
        channel_id,
        message_id,
        ClosedLobbyEmbed {
            title: format!("🚫 {} — {reason}", kind.label()),
            description: format!("{} has been closed.", kind.display_name()),
        },
        true,
    );
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
        results: VecDeque<Result<String, TransportError>>,
    }

    impl RecordingSend {
        fn with_results(
            results: impl IntoIterator<Item = Result<&'static str, TransportError>>,
        ) -> Self {
            Self {
                results: results
                    .into_iter()
                    .map(|result| result.map(str::to_owned))
                    .collect(),
                ..Self::default()
            }
        }
    }

    impl SendPort for RecordingSend {
        fn send(&mut self, payload: SentPayload) -> Result<String, TransportError> {
            self.calls.push(payload);
            self.results
                .pop_front()
                .unwrap_or_else(|| Ok("ok".to_owned()))
        }
    }

    #[test]
    fn test_safe_defer_forwards_thinking_flag() {
        let mut defer = RecordingDefer::default();
        assert!(safe_defer(&mut defer, false, true));
        assert_eq!(defer.calls, [(false, true)]);
    }

    #[test]
    fn test_safe_followup_happy_path_sends_followup() {
        let mut followup = RecordingSend::default();
        let mut ports = InteractionPorts {
            followup: &mut followup,
            channel: None,
        };
        let mut request = SendRequest {
            content: Some("hi".to_owned()),
            ephemeral: true,
            ..SendRequest::default()
        };
        assert_eq!(safe_followup(&mut ports, &mut request).as_deref(), Ok("ok"));
        assert_eq!(followup.calls[0].content.as_deref(), Some("hi"));
        assert!(followup.calls[0].ephemeral);
    }

    #[test]
    fn test_followup_failure_after_defer_falls_back_to_channel() {
        let mut followup =
            RecordingSend::with_results([Err(TransportError::discord("Missing Permissions"))]);
        let mut channel = RecordingSend::with_results([Ok("channel-msg")]);
        let mut ports = InteractionPorts {
            followup: &mut followup,
            channel: Some(&mut channel),
        };
        let mut request = SendRequest {
            content: Some("shop".to_owned()),
            ..SendRequest::default()
        };
        assert_eq!(
            safe_followup(&mut ports, &mut request).as_deref(),
            Ok("channel-msg")
        );
        assert_eq!(channel.calls.len(), 1);
        assert_eq!(channel.calls[0].content.as_deref(), Some("shop"));
    }

    #[test]
    fn test_ephemeral_followup_failure_does_not_leak_to_channel() {
        let mut followup =
            RecordingSend::with_results([Err(TransportError::discord("Missing Permissions"))]);
        let mut channel = RecordingSend::default();
        let mut ports = InteractionPorts {
            followup: &mut followup,
            channel: Some(&mut channel),
        };
        let mut request = SendRequest {
            content: Some("secret".to_owned()),
            ephemeral: true,
            ..SendRequest::default()
        };
        assert!(safe_followup(&mut ports, &mut request).is_err());
        assert!(channel.calls.is_empty());
    }

    #[test]
    fn test_send_public_or_ephemeral_retries_ephemerally_without_attachment() {
        let mut followup = RecordingSend::with_results([
            Err(TransportError::discord("public blocked")),
            Ok("ephemeral-ok"),
        ]);
        let mut ports = InteractionPorts {
            followup: &mut followup,
            channel: None,
        };
        let mut request = SendRequest {
            embed: Some("E".to_owned()),
            attachments: vec![Attachment::new("decoration.png")],
            ..SendRequest::default()
        };
        assert_eq!(
            send_public_or_ephemeral(&mut ports, &mut request).as_deref(),
            Some("ephemeral-ok")
        );
        assert_eq!(
            followup.calls[0].attachments,
            SentAttachments::File("decoration.png".to_owned())
        );
        assert!(followup.calls[1].ephemeral);
        assert_eq!(followup.calls[1].attachments, SentAttachments::None);
    }

    #[test]
    fn test_send_public_or_ephemeral_last_resort_names_the_error() {
        let mut followup = RecordingSend::with_results([
            Err(TransportError::discord("Cannot send embeds here")),
            Err(TransportError::discord("Cannot send embeds here")),
            Ok("note-ok"),
        ]);
        let mut ports = InteractionPorts {
            followup: &mut followup,
            channel: None,
        };
        let mut request = SendRequest {
            embed: Some("E".to_owned()),
            attachments: vec![Attachment::new("decoration.png")],
            ..SendRequest::default()
        };
        assert_eq!(
            send_public_or_ephemeral(&mut ports, &mut request).as_deref(),
            Some("note-ok")
        );
        let note = followup.calls.last().expect("diagnostic note");
        assert!(note.ephemeral);
        assert!(
            note.content
                .as_deref()
                .is_some_and(|content| content.contains("HTTPException"))
        );
    }

    #[test]
    fn test_non_ephemeral_failure_with_no_channel_reraises() {
        let mut followup =
            RecordingSend::with_results([Err(TransportError::discord("Missing Permissions"))]);
        let mut ports = InteractionPorts {
            followup: &mut followup,
            channel: None,
        };
        let mut request = SendRequest {
            content: Some("x".to_owned()),
            ..SendRequest::default()
        };
        assert!(safe_followup(&mut ports, &mut request).is_err());
    }

    #[test]
    fn test_channel_fallback_rewinds_and_forwards_attachment() {
        let mut followup =
            RecordingSend::with_results([Err(TransportError::discord("Missing Permissions"))]);
        let mut channel = RecordingSend::with_results([Ok("channel-msg")]);
        let mut ports = InteractionPorts {
            followup: &mut followup,
            channel: Some(&mut channel),
        };
        let mut request = SendRequest {
            embed: Some("E".to_owned()),
            attachments: vec![Attachment::new("result.png")],
            ..SendRequest::default()
        };
        assert_eq!(
            safe_followup(&mut ports, &mut request).as_deref(),
            Ok("channel-msg")
        );
        assert_eq!(request.attachments[0].reset_count, 1);
        assert_eq!(
            channel.calls[0].attachments,
            SentAttachments::File("result.png".to_owned())
        );
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

    #[derive(Default)]
    struct RecordingLobbyClose {
        fetches: Vec<i64>,
        edits: Vec<(i64, i64, ClosedLobbyEmbed, bool)>,
    }

    impl LobbyClosePort for RecordingLobbyClose {
        fn lobby_message_id(&self, _guild_id: Option<i64>, _kind: LobbyKind) -> Option<i64> {
            Some(123)
        }

        fn lobby_channel_id(&self, _guild_id: Option<i64>, _kind: LobbyKind) -> Option<i64> {
            Some(456)
        }

        fn has_cached_channel(&self, channel_id: i64) -> bool {
            channel_id == 456
        }

        fn fetch_channel(&mut self, channel_id: i64) -> Result<(), TransportError> {
            self.fetches.push(channel_id);
            Ok(())
        }

        fn edit_partial_message(
            &mut self,
            channel_id: i64,
            message_id: i64,
            embed: ClosedLobbyEmbed,
            clear_view: bool,
        ) -> Result<(), TransportError> {
            self.edits.push((channel_id, message_id, embed, clear_view));
            Ok(())
        }
    }

    #[test]
    fn test_close_lobby_edits_partial_message_without_fetch() {
        let mut port = RecordingLobbyClose::default();
        update_lobby_message_closed(&mut port, "Lobby Reset", Some(42), LobbyKind::Open);
        assert!(port.fetches.is_empty());
        assert_eq!(port.edits.len(), 1);
        assert_eq!(port.edits[0].0, 456);
        assert_eq!(port.edits[0].1, 123);
        assert_eq!(
            port.edits[0].2.title,
            "🚫 🍽️ All You Can Feed — Lobby Reset"
        );
        assert!(port.edits[0].3);
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
