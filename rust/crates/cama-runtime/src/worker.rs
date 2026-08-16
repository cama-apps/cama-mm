use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::watch;

use crate::gateway::ReconnectPolicy;

#[derive(Clone)]
pub struct WorkerContext {
    shutdown: watch::Receiver<bool>,
}

impl WorkerContext {
    pub(crate) const fn new(shutdown: watch::Receiver<bool>) -> Self {
        Self { shutdown }
    }

    #[must_use]
    pub fn shutdown_requested(&self) -> bool {
        *self.shutdown.borrow()
    }

    /// Wait for shutdown and return. Long-lived workers should select this
    /// against provider I/O so process termination never depends on a polling
    /// interval elapsing.
    pub async fn cancelled(&mut self) {
        while !*self.shutdown.borrow() {
            if self.shutdown.changed().await.is_err() {
                break;
            }
        }
    }

    /// Sleep for a worker interval, returning `false` if shutdown won the race.
    pub async fn sleep(&mut self, duration: Duration) -> bool {
        if self.shutdown_requested() {
            return false;
        }
        tokio::select! {
            () = tokio::time::sleep(duration) => true,
            result = self.shutdown.changed() => result.is_ok() && !*self.shutdown.borrow(),
        }
    }
}

#[async_trait]
pub trait BackgroundWorker: Send + Sync {
    /// Run until clean completion, shutdown, or a recoverable failure. Returning
    /// an error invokes the supervisor's capped exponential backoff.
    async fn run(&self, context: WorkerContext) -> Result<(), String>;
}

#[derive(Clone)]
pub struct BackgroundWorkerSpec {
    pub(crate) name: String,
    pub(crate) worker: Arc<dyn BackgroundWorker>,
    pub(crate) restart: ReconnectPolicy,
}

impl BackgroundWorkerSpec {
    #[must_use]
    pub fn new(name: impl Into<String>, worker: Arc<dyn BackgroundWorker>) -> Self {
        Self {
            name: name.into(),
            worker,
            restart: ReconnectPolicy::new(Duration::from_secs(5), Duration::from_secs(300)),
        }
    }

    #[must_use]
    pub const fn restart_policy(mut self, restart: ReconnectPolicy) -> Self {
        self.restart = restart;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn context_sleep_is_cancelled_immediately() {
        let (sender, receiver) = watch::channel(false);
        let mut context = WorkerContext::new(receiver);
        sender.send(true).expect("signal shutdown");
        assert!(!context.sleep(Duration::from_secs(60)).await);
        assert!(context.shutdown_requested());
    }
}
