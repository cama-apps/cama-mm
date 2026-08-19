//! Transport-neutral raw Discord reaction fan-out.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawReactionKind {
    Add,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawReactionEmoji {
    pub id: Option<u64>,
    pub name: String,
    pub animated: bool,
}

impl RawReactionEmoji {
    #[must_use]
    pub fn unicode(name: impl Into<String>) -> Self {
        Self {
            id: None,
            name: name.into(),
            animated: false,
        }
    }

    #[must_use]
    pub fn custom(id: u64, name: impl Into<String>, animated: bool) -> Self {
        Self {
            id: Some(id),
            name: name.into(),
            animated,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawReactionEvent {
    pub kind: RawReactionKind,
    pub guild_id: Option<u64>,
    pub channel_id: u64,
    pub message_id: u64,
    pub user_id: u64,
    /// Present for reaction-add when Discord includes the guild member.
    pub actor_is_bot: Option<bool>,
    pub actor_display_name: Option<String>,
    pub emoji: RawReactionEmoji,
}

#[async_trait]
pub trait RawReactionObserver: Send + Sync {
    fn name(&self) -> &'static str;

    async fn observe(&self, event: RawReactionEvent) -> Result<(), String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawReactionObserverFailure {
    pub observer: &'static str,
    pub message: String,
}

#[derive(Clone, Default)]
pub struct RawReactionObservers {
    observers: Arc<[Arc<dyn RawReactionObserver>]>,
}

impl RawReactionObservers {
    #[must_use]
    pub fn new(observers: Vec<Arc<dyn RawReactionObserver>>) -> Self {
        Self {
            observers: observers.into(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.observers.is_empty()
    }

    pub async fn dispatch(&self, event: RawReactionEvent) -> Vec<RawReactionObserverFailure> {
        let mut failures = Vec::new();
        for observer in self.observers.iter() {
            if let Err(message) = observer.observe(event.clone()).await {
                failures.push(RawReactionObserverFailure {
                    observer: observer.name(),
                    message,
                });
            }
        }
        failures
    }
}

impl fmt::Debug for RawReactionObservers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawReactionObservers")
            .field(
                "observers",
                &self
                    .observers
                    .iter()
                    .map(|observer| observer.name())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct Recorder {
        name: &'static str,
        events: Arc<Mutex<Vec<RawReactionEvent>>>,
        failure: Option<&'static str>,
    }

    #[async_trait]
    impl RawReactionObserver for Recorder {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn observe(&self, event: RawReactionEvent) -> Result<(), String> {
            self.events.lock().expect("events").push(event);
            self.failure
                .map_or(Ok(()), |message| Err(message.to_owned()))
        }
    }

    #[tokio::test]
    async fn fanout_preserves_typed_add_remove_identity_and_isolates_failures() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observers = RawReactionObservers::new(vec![
            Arc::new(Recorder {
                name: "failing",
                events: Arc::clone(&events),
                failure: Some("offline failure"),
            }),
            Arc::new(Recorder {
                name: "healthy",
                events: Arc::clone(&events),
                failure: None,
            }),
        ]);
        let event = RawReactionEvent {
            kind: RawReactionKind::Add,
            guild_id: Some(1),
            channel_id: 2,
            message_id: 3,
            user_id: 4,
            actor_is_bot: Some(false),
            actor_display_name: Some("Server Nick".to_owned()),
            emoji: RawReactionEmoji::custom(5, "jopacoin", true),
        };

        let failures = observers.dispatch(event.clone()).await;
        assert_eq!(*events.lock().expect("events"), vec![event.clone(), event]);
        assert_eq!(
            failures,
            [RawReactionObserverFailure {
                observer: "failing",
                message: "offline failure".to_owned(),
            }]
        );
    }
}
