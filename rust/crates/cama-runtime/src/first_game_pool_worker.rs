//! Production worker for daily first-game betting-pool funding.
//!
//! The schema-admitted SQLite repository performs atomic, idempotent funding.
//! A tiny `app_kv` outbox is written in that same transaction so a process
//! crash or Discord edit failure after commit cannot silently lose the lobby
//! display refresh. Every repository call runs on Tokio's blocking pool.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_db::dota_bet_seed_repository::{
    DotaBetSeedRepository, FirstGamePoolRefresh, FundingOutcome,
};
use tracing::{info, warn};

use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const FIRST_GAME_POOL_WORKER_NAME: &str = "first_game_pool";
pub const FIRST_GAME_POOL_WAKE_INTERVAL: Duration = Duration::from_secs(900);
pub const FIRST_GAME_POOL_REFRESH_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const REFRESH_ATTEMPTS: usize = 2;

/// Current guild membership supplied by the live gateway cache.
pub trait FirstGamePoolGuildSource: Send + Sync {
    fn live_guild_ids(&self) -> Result<Vec<i64>, String>;
}

/// Refresh both live lobby surfaces for one guild. Missing/closed lobbies are
/// successful no-ops; an error means the durable outbox row must remain.
#[async_trait]
pub trait FirstGamePoolDisplayPort: Send + Sync {
    async fn refresh_first_game_pool_lobbies(&self, guild_id: i64) -> Result<(), String>;
}

trait GameDateClock: Send + Sync {
    fn game_date(&self) -> String;
}

struct FixedPstGameDateClock;

impl GameDateClock for FixedPstGameDateClock {
    fn game_date(&self) -> String {
        cama_domain::game_date::get_game_date()
    }
}

pub struct FirstGamePoolWorker {
    repository: DotaBetSeedRepository,
    daily_amount: i64,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    display: Arc<dyn FirstGamePoolDisplayPort>,
    clock: Arc<dyn GameDateClock>,
    wake_interval: Duration,
    refresh_retry_interval: Duration,
}

impl FirstGamePoolWorker {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        daily_amount: i64,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
        display: Arc<dyn FirstGamePoolDisplayPort>,
    ) -> Self {
        Self {
            repository: DotaBetSeedRepository::new(database_path),
            daily_amount,
            guild_source,
            display,
            clock: Arc::new(FixedPstGameDateClock),
            wake_interval: FIRST_GAME_POOL_WAKE_INTERVAL,
            refresh_retry_interval: FIRST_GAME_POOL_REFRESH_RETRY_INTERVAL,
        }
    }

    #[cfg(all(test, feature = "runtime-test-dig"))]
    fn with_clock_and_intervals(
        database_path: impl AsRef<Path>,
        daily_amount: i64,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
        display: Arc<dyn FirstGamePoolDisplayPort>,
        clock: Arc<dyn GameDateClock>,
        wake_interval: Duration,
        refresh_retry_interval: Duration,
    ) -> Self {
        Self {
            repository: DotaBetSeedRepository::new(database_path),
            daily_amount,
            guild_source,
            display,
            clock,
            wake_interval,
            refresh_retry_interval,
        }
    }

    async fn fund_guild(&self, guild_id: i64, game_date: String) -> Result<FundingOutcome, String> {
        let repository = self.repository.clone();
        let daily_amount = self.daily_amount;
        tokio::task::spawn_blocking(move || {
            repository
                .fund_first_game_pools_and_queue_refresh(Some(guild_id), &game_date, daily_amount)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("first-game pool funding task failed: {error}"))?
    }

    async fn pending_refreshes(&self) -> Result<Vec<FirstGamePoolRefresh>, String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            repository
                .pending_first_game_pool_refreshes()
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("first-game pool outbox read task failed: {error}"))?
    }

    async fn acknowledge_refresh(&self, refresh: FirstGamePoolRefresh) -> Result<bool, String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            repository
                .acknowledge_first_game_pool_refresh(&refresh)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("first-game pool outbox acknowledgement task failed: {error}"))?
    }

    async fn wake_once(&self, context: &mut WorkerContext) -> Result<(), String> {
        let live_guilds = self
            .guild_source
            .live_guild_ids()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let game_date = self.clock.game_date();
        let mut failures = Vec::new();

        for &guild_id in &live_guilds {
            if context.shutdown_requested() {
                return Ok(());
            }
            match self.fund_guild(guild_id, game_date.clone()).await {
                Ok(outcome) => {
                    if !outcome.funded_dates.is_empty() {
                        info!(
                            guild_id,
                            ?outcome.funded_dates,
                            open_pool = outcome.balances.open,
                            low_skill_pool = outcome.balances.low_skill,
                            "funded first-game betting pools"
                        );
                    }
                }
                Err(error) => {
                    warn!(guild_id, %error, "first-game pool funding failed");
                    failures.push(format!("guild {guild_id} funding: {error}"));
                }
            }
        }

        let pending = self.pending_refreshes().await?;
        for refresh in pending
            .into_iter()
            .filter(|refresh| live_guilds.contains(&refresh.guild_id))
        {
            if context.shutdown_requested() {
                return Ok(());
            }
            let mut delivered = false;
            let mut final_error = None;
            for attempt in 0..REFRESH_ATTEMPTS {
                let result = tokio::select! {
                    result = self.display.refresh_first_game_pool_lobbies(refresh.guild_id) => result,
                    () = context.cancelled() => return Ok(()),
                };
                match result {
                    Ok(()) => {
                        delivered = true;
                        break;
                    }
                    Err(error) => final_error = Some(error),
                }
                if attempt + 1 < REFRESH_ATTEMPTS
                    && !context.sleep(self.refresh_retry_interval).await
                {
                    return Ok(());
                }
            }
            if delivered {
                if let Err(error) = self.acknowledge_refresh(refresh.clone()).await {
                    warn!(
                        guild_id = refresh.guild_id,
                        generation = refresh.generation,
                        %error,
                        "first-game pool display acknowledgement failed"
                    );
                    failures.push(format!(
                        "guild {} display acknowledgement generation {}: {error}",
                        refresh.guild_id, refresh.generation
                    ));
                }
            } else {
                let error = final_error.unwrap_or_else(|| "unknown display failure".to_owned());
                warn!(
                    guild_id = refresh.guild_id,
                    generation = refresh.generation,
                    %error,
                    "first-game pool display remains pending"
                );
                failures.push(format!(
                    "guild {} display generation {}: {error}",
                    refresh.guild_id, refresh.generation
                ));
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

/// Build the production spec only when daily funding is enabled. The caller
/// attaches the returned spec to `Runtime`, which retains and supervises it.
#[must_use]
pub fn first_game_pool_worker_spec(
    database_path: impl AsRef<Path>,
    daily_amount: i64,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    display: Arc<dyn FirstGamePoolDisplayPort>,
) -> Option<BackgroundWorkerSpec> {
    (daily_amount > 0).then(|| {
        BackgroundWorkerSpec::new(
            FIRST_GAME_POOL_WORKER_NAME,
            Arc::new(FirstGamePoolWorker::new(
                database_path,
                daily_amount,
                guild_source,
                display,
            )),
        )
    })
}

#[async_trait]
impl BackgroundWorker for FirstGamePoolWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }
            match self.wake_once(&mut context).await {
                Ok(()) => {}
                Err(_) if context.shutdown_requested() => return Ok(()),
                Err(error) => return Err(error),
            }
            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

#[cfg(all(test, feature = "runtime-test-dig"))]
#[path = "first_game_pool_worker/tests.rs"]
mod tests;
