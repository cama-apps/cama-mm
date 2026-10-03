//! Production worker that removes lobby members who have not confirmed a
//! ready check five minutes after it was posted or refreshed, and retires
//! that ready check. The policy and the removal live in
//! [`crate::lobby_provider`]; this worker only supplies the wake-up.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::info;

use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const READYCHECK_SWEEP_WORKER_NAME: &str = "readycheck_sweep";
/// How late past its deadline a sweep can run.
pub const READYCHECK_SWEEP_WAKE_INTERVAL: Duration = Duration::from_secs(15);

/// Sweep every ready check whose deadline has passed, returning how many
/// players were removed. Implemented by
/// [`crate::lobby_provider::LobbyRegistrationProvider`].
#[async_trait]
pub trait ReadycheckSweepPort: Send + Sync {
    async fn sweep_due_readychecks(&self) -> usize;
}

pub struct ReadycheckSweepWorker {
    sweep: Arc<dyn ReadycheckSweepPort>,
    wake_interval: Duration,
}

impl ReadycheckSweepWorker {
    #[must_use]
    pub fn new(sweep: Arc<dyn ReadycheckSweepPort>) -> Self {
        Self {
            sweep,
            wake_interval: READYCHECK_SWEEP_WAKE_INTERVAL,
        }
    }
}

/// Build the production worker specification retained by [`crate::Runtime`].
#[must_use]
pub fn readycheck_sweep_worker_spec(sweep: Arc<dyn ReadycheckSweepPort>) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new(
        READYCHECK_SWEEP_WORKER_NAME,
        Arc::new(ReadycheckSweepWorker::new(sweep)),
    )
}

#[async_trait]
impl BackgroundWorker for ReadycheckSweepWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }

            let removed = self.sweep.sweep_due_readychecks().await;
            if removed > 0 {
                info!(
                    removed,
                    "removed lobby members who did not confirm a ready check"
                );
            }

            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
#[path = "readycheck_sweep_worker/tests.rs"]
mod tests;
