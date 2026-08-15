use std::collections::BTreeSet;
use std::sync::Mutex;

use async_trait::async_trait;

use super::*;

struct RecordingPins {
    messages: Mutex<Result<Vec<DiscordPinnedMessage>, String>>,
    failures: Mutex<BTreeSet<u64>>,
    attempts: Mutex<Vec<(u64, u64)>>,
}

impl Default for RecordingPins {
    fn default() -> Self {
        Self {
            messages: Mutex::new(Ok(Vec::new())),
            failures: Mutex::new(BTreeSet::new()),
            attempts: Mutex::new(Vec::new()),
        }
    }
}

impl RecordingPins {
    fn with_messages(messages: Vec<DiscordPinnedMessage>) -> Self {
        Self {
            messages: Mutex::new(Ok(messages)),
            ..Self::default()
        }
    }

    fn attempts(&self) -> Vec<(u64, u64)> {
        self.attempts.lock().expect("pin attempts").clone()
    }
}

#[async_trait]
impl PinManagementPort for RecordingPins {
    async fn pinned_messages(&self, _channel_id: u64) -> Result<Vec<DiscordPinnedMessage>, String> {
        self.messages.lock().expect("pinned messages").clone()
    }

    async fn unpin_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        self.attempts
            .lock()
            .expect("pin attempts")
            .push((channel_id, message_id));
        if self
            .failures
            .lock()
            .expect("pin failures")
            .contains(&message_id)
        {
            Err("Discord rejected unpin".to_owned())
        } else {
            Ok(())
        }
    }
}

fn pinned(message_id: u64, author_id: u64) -> DiscordPinnedMessage {
    DiscordPinnedMessage {
        message_id,
        author_id,
    }
}

#[tokio::test]
async fn test_unpins_all_bot_messages() {
    let port = RecordingPins::with_messages(vec![
        pinned(100, 12_345),
        pinned(101, 12_345),
        pinned(200, 99_999),
    ]);

    assert_eq!(
        safe_unpin_all_bot_messages(Some(&port), Some(77), 12_345).await,
        2
    );
    assert_eq!(port.attempts(), [(77, 100), (77, 101)]);
}

#[tokio::test]
async fn test_returns_zero_for_no_bot_messages() {
    let port = RecordingPins::with_messages(vec![pinned(200, 99_999)]);
    assert_eq!(
        safe_unpin_all_bot_messages(Some(&port), Some(77), 12_345).await,
        0
    );
    assert!(port.attempts().is_empty());
}

#[tokio::test]
async fn test_returns_zero_for_no_channel() {
    let port = RecordingPins::with_messages(vec![pinned(100, 12_345)]);
    assert_eq!(
        safe_unpin_all_bot_messages(Some(&port), None, 12_345).await,
        0
    );
    assert!(port.attempts().is_empty());
}

#[tokio::test]
async fn test_returns_zero_for_channel_without_pins() {
    assert_eq!(
        safe_unpin_all_bot_messages::<RecordingPins>(None, Some(77), 12_345).await,
        0
    );
}

#[tokio::test]
async fn test_handles_fetch_pins_error_gracefully() {
    let port = RecordingPins::default();
    *port.messages.lock().expect("pinned messages") = Err("fetch failed".to_owned());
    assert_eq!(
        safe_unpin_all_bot_messages(Some(&port), Some(77), 12_345).await,
        0
    );
    assert!(port.attempts().is_empty());
}

#[tokio::test]
async fn test_handles_forbidden_on_unpin() {
    let port = RecordingPins::with_messages(vec![pinned(100, 12_345), pinned(101, 12_345)]);
    port.failures.lock().expect("pin failures").insert(100);
    assert_eq!(
        safe_unpin_all_bot_messages(Some(&port), Some(77), 12_345).await,
        1
    );
    assert_eq!(port.attempts(), [(77, 100), (77, 101)]);
}

#[tokio::test]
async fn test_handles_discord_exception_on_unpin() {
    let port = RecordingPins::with_messages(vec![pinned(100, 12_345), pinned(101, 12_345)]);
    port.failures.lock().expect("pin failures").insert(100);
    assert_eq!(
        safe_unpin_all_bot_messages(Some(&port), Some(77), 12_345).await,
        1
    );
    assert_eq!(port.attempts(), [(77, 100), (77, 101)]);
}

#[tokio::test]
async fn test_empty_pins_list() {
    let port = RecordingPins::default();
    assert_eq!(
        safe_unpin_all_bot_messages(Some(&port), Some(77), 12_345).await,
        0
    );
    assert!(port.attempts().is_empty());
}

#[tokio::test]
async fn test_safe_unpin_message_only_targets_tracked_message() {
    let port = RecordingPins::with_messages(vec![pinned(100, 12_345), pinned(101, 12_345)]);
    assert!(safe_unpin_message(Some(&port), Some(77), Some(101)).await);
    assert_eq!(port.attempts(), [(77, 101)]);
}
