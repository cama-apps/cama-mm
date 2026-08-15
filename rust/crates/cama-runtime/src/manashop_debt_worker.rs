//! Production worker for settling expired mana-shop Dark Bargain debt.
//!
//! The runtime supervisor starts workers only after Discord emits `Ready`.
//! This worker performs one catch-up settlement immediately, then sleeps for
//! the Python-compatible hourly interval. Repository calls always run on the
//! blocking pool so SQLite cannot stall the Tokio executor.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_db::manashop_rework_repository::{DarkBargainSettlement, ManashopRepository};
use chrono::Utc;
use tracing::info;

use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const MANASHOP_DEBT_WORKER_NAME: &str = "manashop_debt";
pub const MANASHOP_DEBT_WAKE_INTERVAL: Duration = Duration::from_secs(3_600);

trait UnixClock: Send + Sync {
    fn now(&self) -> i64;
}

struct UtcClock;

impl UnixClock for UtcClock {
    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

/// Hourly production adapter around the repository's atomic, idempotent debt
/// settlement operation.
pub struct ManashopDebtWorker {
    repository: ManashopRepository,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    clock: Arc<dyn UnixClock>,
    wake_interval: Duration,
}

impl ManashopDebtWorker {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
    ) -> Self {
        Self {
            repository: ManashopRepository::new(database_path),
            guild_source,
            clock: Arc::new(UtcClock),
            wake_interval: MANASHOP_DEBT_WAKE_INTERVAL,
        }
    }

    async fn settle_once(&self) -> Result<Vec<DarkBargainSettlement>, String> {
        let repository = self.repository.clone();
        let now = self.clock.now();
        let guild_ids = self
            .guild_source
            .live_guild_ids()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        tokio::task::spawn_blocking(move || {
            repository.settle_due_dark_bargains_for_guilds(now, &guild_ids)
        })
        .await
        .map_err(|error| format!("manashop debt blocking task failed: {error}"))?
        .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    fn with_clock_and_interval(
        database_path: impl AsRef<Path>,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
        clock: Arc<dyn UnixClock>,
        wake_interval: Duration,
    ) -> Self {
        Self {
            repository: ManashopRepository::new(database_path),
            guild_source,
            clock,
            wake_interval,
        }
    }
}

/// Build the production worker specification retained by [`crate::Runtime`].
#[must_use]
pub fn manashop_debt_worker_spec(
    database_path: impl AsRef<Path>,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new(
        MANASHOP_DEBT_WORKER_NAME,
        Arc::new(ManashopDebtWorker::new(database_path, guild_source)),
    )
}

#[async_trait]
impl BackgroundWorker for ManashopDebtWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }

            let settlements = match self.settle_once().await {
                Ok(settlements) => settlements,
                Err(_) if context.shutdown_requested() => return Ok(()),
                Err(error) => return Err(error),
            };
            if !settlements.is_empty() {
                info!(
                    settlement_count = settlements.len(),
                    ?settlements,
                    "settled due Dark Bargain debts"
                );
            }

            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
#[path = "manashop_debt_worker/tests.rs"]
mod tests;
