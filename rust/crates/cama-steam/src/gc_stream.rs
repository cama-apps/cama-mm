//! Shared handling for steam-vent Game Coordinator subscriptions.

use futures_util::{Stream, StreamExt};
use steam_vent::NetworkError;
use tokio::task::JoinHandle;

/// Forward a steam-vent subscription into an unbounded queue as soon as each
/// message arrives.
///
/// steam-vent buffers only 16 messages per kind and reports overflow as
/// [`NetworkError::EOF`], so a subscription left unread across a handshake or
/// lock wait loses messages. The forwarder does no other work, so the bounded
/// upstream broadcast cannot overflow while the consumer is busy. The caller
/// owns the returned task and must abort it when the client is dropped.
pub(crate) fn buffer_gc_messages<T: Send + 'static>(
    messages: impl Stream<Item = Result<T, NetworkError>> + Send + 'static,
) -> (
    impl Stream<Item = Result<T, NetworkError>> + Send + 'static,
    JoinHandle<()>,
) {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        futures_util::pin_mut!(messages);
        while let Some(message) = messages.next().await {
            if sender.send(message).is_err() {
                return;
            }
        }
    });
    let stream = futures_util::stream::poll_fn(move |cx| receiver.poll_recv(cx));
    (stream, task)
}
