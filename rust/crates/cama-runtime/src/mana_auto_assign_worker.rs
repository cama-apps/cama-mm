//! Production worker that grants every registered player their daily mana
//! land assignment even if they never run `/mana`.
//!
//! Mirrors `/mana all`'s `board_command` behavior exactly: pulls DB-registered
//! players per guild, skips anyone already assigned today, weighted-random
//! assigns a land, pays the Plains White stipend, and reconciles Guardian
//! refunds — all via `ManaService::assign_all_daily_mana_with_board`, which is
//! naturally idempotent. Runs immediately on start, then sleeps.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_app::mana_service::AssignmentBoard;
use cama_db::mana_service_repository::ManaRepository;
use tracing::info;

use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::mana_provider::{
    ManaClock, ManaDiscordPort, SystemManaClock, live_service, pay_batch_stipends_sqlite,
};
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const MANA_AUTO_ASSIGN_WORKER_NAME: &str = "mana_auto_assign";
pub const MANA_AUTO_ASSIGN_WAKE_INTERVAL: Duration = Duration::from_secs(900);

pub struct ManaAutoAssignWorker {
    database_path: PathBuf,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    discord: Arc<dyn ManaDiscordPort>,
    clock: Arc<dyn ManaClock>,
    white_stipend: i64,
    wake_interval: Duration,
}

impl ManaAutoAssignWorker {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        white_stipend: i64,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
        discord: Arc<dyn ManaDiscordPort>,
    ) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
            guild_source,
            discord,
            clock: Arc::new(SystemManaClock),
            white_stipend,
            wake_interval: MANA_AUTO_ASSIGN_WAKE_INTERVAL,
        }
    }

    async fn assign_once(&self) -> Result<Vec<AssignmentBoard>, String> {
        let guild_ids = self.guild_source.live_guild_ids()?;
        let moment = self.clock.moment()?;
        let today = moment.today()?;
        let mut boards = Vec::with_capacity(guild_ids.len());
        for guild_id in guild_ids {
            let members = self.discord.mana_guild_members(guild_id)?;
            let ash_fan_ids = members
                .iter()
                .filter(|member| member.is_ash_fan())
                .map(|member| member.user_id)
                .collect::<BTreeSet<_>>();

            let path = self.database_path.clone();
            let today_for_task = today.clone();
            let day_start_ts = moment.day_start()?;
            let board = tokio::task::spawn_blocking(move || {
                let repository = ManaRepository::new(&path);
                let mut service = live_service(repository, &path, moment);
                service.assign_all_daily_mana_with_board(
                    guild_id,
                    &today_for_task,
                    day_start_ts,
                    &ash_fan_ids,
                )
            })
            .await
            .map_err(|error| format!("mana auto-assign blocking task failed: {error}"))?
            .map_err(|error| error.to_string())?;

            pay_batch_stipends_sqlite(
                self.database_path.clone(),
                &board,
                guild_id,
                self.white_stipend,
            )
            .await;

            if !board.assignments.is_empty() {
                info!(
                    guild_id,
                    assigned_count = board.assignments.len(),
                    "auto-assigned daily mana for players who had not run /mana"
                );
            }
            boards.push(board);
        }
        Ok(boards)
    }

    #[cfg(all(test, feature = "runtime-test-core"))]
    fn with_clock_and_interval(
        database_path: impl AsRef<Path>,
        white_stipend: i64,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
        discord: Arc<dyn ManaDiscordPort>,
        clock: Arc<dyn ManaClock>,
        wake_interval: Duration,
    ) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
            guild_source,
            discord,
            clock,
            white_stipend,
            wake_interval,
        }
    }
}

#[must_use]
pub fn mana_auto_assign_worker_spec(
    database_path: impl AsRef<Path>,
    white_stipend: i64,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    discord: Arc<dyn ManaDiscordPort>,
) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new(
        MANA_AUTO_ASSIGN_WORKER_NAME,
        Arc::new(ManaAutoAssignWorker::new(
            database_path,
            white_stipend,
            guild_source,
            discord,
        )),
    )
}

#[async_trait]
impl BackgroundWorker for ManaAutoAssignWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }
            match self.assign_once().await {
                Ok(_) => {}
                Err(_) if context.shutdown_requested() => return Ok(()),
                Err(error) => return Err(error),
            }
            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "mana_auto_assign_worker/tests.rs"]
mod tests;
