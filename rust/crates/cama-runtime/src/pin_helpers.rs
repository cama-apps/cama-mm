//! Fail-soft Discord pin cleanup shared by lobby-style runtime providers.
//!
//! The bulk cleanup deliberately filters by author before issuing any unpin
//! request. A failure for one message is isolated so later bot pins are still
//! cleaned up, matching the Python restart-recovery utility.

use async_trait::async_trait;

use crate::discord_transport::{DiscordPinnedMessage, DiscordTransport};

#[async_trait]
pub trait PinManagementPort: Send + Sync {
    async fn pinned_messages(&self, channel_id: u64) -> Result<Vec<DiscordPinnedMessage>, String>;

    async fn unpin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String>;
}

#[async_trait]
impl<T> PinManagementPort for T
where
    T: DiscordTransport + ?Sized,
{
    async fn pinned_messages(&self, channel_id: u64) -> Result<Vec<DiscordPinnedMessage>, String> {
        DiscordTransport::pinned_messages(self, channel_id).await
    }

    async fn unpin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        DiscordTransport::unpin_message(self, channel_id, message_id).await
    }
}

/// Unpin one explicitly tracked message without inspecting or disturbing any
/// sibling pins. Missing identifiers and Discord failures are fail-soft.
pub async fn safe_unpin_message<P>(
    port: Option<&P>,
    channel_id: Option<u64>,
    message_id: Option<u64>,
) -> bool
where
    P: PinManagementPort + ?Sized,
{
    let (Some(port), Some(channel_id), Some(message_id)) = (port, channel_id, message_id) else {
        return false;
    };
    port.unpin_message(channel_id, message_id).await.is_ok()
}

/// Unpin every currently pinned message authored by `bot_user_id`.
///
/// Fetch failures return zero. Individual unpin failures do not stop cleanup,
/// and the returned count includes only successful operations.
pub async fn safe_unpin_all_bot_messages<P>(
    port: Option<&P>,
    channel_id: Option<u64>,
    bot_user_id: u64,
) -> usize
where
    P: PinManagementPort + ?Sized,
{
    let (Some(port), Some(channel_id)) = (port, channel_id) else {
        return 0;
    };
    let Ok(messages) = port.pinned_messages(channel_id).await else {
        return 0;
    };

    let mut unpinned = 0;
    for message in messages
        .into_iter()
        .filter(|message| message.author_id == bot_user_id)
    {
        if port
            .unpin_message(channel_id, message.message_id)
            .await
            .is_ok()
        {
            unpinned += 1;
        }
    }
    unpinned
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests;
