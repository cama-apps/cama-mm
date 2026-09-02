//! Production worker that grants every registered player their daily mana
//! land assignment even if they never run `/mana`.
//!
//! Mirrors `/mana all`'s `board_command` behavior exactly: pulls DB-registered
//! players per guild, skips anyone already assigned today, weighted-random
//! assigns a land, pays the Plains White stipend inside the same claim
//! transaction, and reconciles Guardian refunds — all via
//! `ManaService::assign_all_daily_mana_with_board`, which is naturally
//! idempotent. Runs immediately on start, then sleeps.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cama_app::mana_service::AssignmentBoard;
use cama_db::mana_service_repository::ManaRepository;
use tracing::{info, warn};

use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::mana_provider::{ManaClock, ManaDiscordPort, ManaMoment, SystemManaClock, live_service};
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const MANA_AUTO_ASSIGN_WORKER_NAME: &str = "mana_auto_assign";
pub const MANA_AUTO_ASSIGN_WAKE_INTERVAL: Duration = Duration::from_secs(900);

/// Cold-cache deferrals tolerated per guild before escalating to a warning:
/// more than this many consecutive empty member lists (over an hour at the
/// 900-second cadence) means the cache is not merely cold — the members intent
/// may be revoked or chunking broken — and operators should see it.
const COLD_CACHE_WARN_DEFERRALS: u32 = 4;

pub struct ManaAutoAssignWorker {
    database_path: PathBuf,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    discord: Arc<dyn ManaDiscordPort>,
    clock: Arc<dyn ManaClock>,
    white_stipend: i64,
    wake_interval: Duration,
    cold_cache_deferrals: Mutex<BTreeMap<i64, u32>>,
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
            cold_cache_deferrals: Mutex::new(BTreeMap::new()),
        }
    }

    async fn assign_once(&self) -> Result<Vec<AssignmentBoard>, String> {
        let guild_ids = self.guild_source.live_guild_ids()?;
        let moment = self.clock.moment()?;
        let today = moment.today()?;
        let guild_count = guild_ids.len();
        let mut boards = Vec::with_capacity(guild_count);
        let mut failures = Vec::new();
        for guild_id in guild_ids {
            match self.assign_guild(guild_id, &today, moment).await {
                Ok(Some(board)) => {
                    if !board.assignments.is_empty() {
                        info!(
                            guild_id,
                            assigned_count = board.assignments.len(),
                            "auto-assigned daily mana for players who had not run /mana"
                        );
                    }
                    boards.push(board);
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(
                        guild_id,
                        %error,
                        "daily mana auto-assign failed for guild; continuing with the rest"
                    );
                    failures.push(format!("guild {guild_id}: {error}"));
                }
            }
        }
        // Every guild gets its turn before the pass reports failure, so one
        // bad guild cannot starve the rest — but any genuine failure still
        // surfaces to the supervisor (assignment is idempotent, so the
        // restart is harmless) and degrades health for operators.
        if !failures.is_empty() {
            return Err(format!(
                "mana auto-assign failed for {} of {guild_count} guilds: {}",
                failures.len(),
                failures.join("; ")
            ));
        }
        Ok(boards)
    }

    /// Assign one guild's daily mana. Returns `Ok(None)` without claiming when
    /// the guild's member list is empty: every guild has at least one member,
    /// so an empty list means Serenity's member cache has not warmed up yet,
    /// and claiming now would permanently bake `is_ash_fan = false` into the
    /// day's weighting (the claim is idempotent per day and cannot
    /// self-correct). A later wake retries once members are visible;
    /// deferrals persisting past [`COLD_CACHE_WARN_DEFERRALS`] consecutive
    /// wakes escalate to a warning so a structurally empty member list
    /// (revoked members intent, broken chunking) stays visible to operators.
    async fn assign_guild(
        &self,
        guild_id: i64,
        today: &str,
        moment: ManaMoment,
    ) -> Result<Option<AssignmentBoard>, String> {
        let members = self.discord.mana_guild_members(guild_id)?;
        if members.is_empty() {
            let streak = {
                let mut streaks = self
                    .cold_cache_deferrals
                    .lock()
                    .map_err(|_| "cold-cache deferral lock was poisoned".to_owned())?;
                let streak = streaks.entry(guild_id).or_insert(0);
                *streak = streak.saturating_add(1);
                *streak
            };
            if streak > COLD_CACHE_WARN_DEFERRALS {
                warn!(
                    guild_id,
                    consecutive_deferrals = streak,
                    "guild member list has stayed empty for over an hour; daily mana remains \
                     deferred (members intent revoked or member chunking broken?)"
                );
            } else {
                info!(
                    guild_id,
                    "guild member cache is empty (cold cache); deferring daily mana auto-assign"
                );
            }
            return Ok(None);
        }
        self.cold_cache_deferrals
            .lock()
            .map_err(|_| "cold-cache deferral lock was poisoned".to_owned())?
            .remove(&guild_id);
        let ash_fan_ids = members
            .iter()
            .filter(|member| member.is_ash_fan())
            .map(|member| member.user_id)
            .collect::<BTreeSet<_>>();

        let path = self.database_path.clone();
        let today_for_task = today.to_owned();
        let day_start_ts = moment.day_start()?;
        let white_stipend = self.white_stipend;
        tokio::task::spawn_blocking(move || {
            let repository = ManaRepository::new(&path);
            let mut service = live_service(repository, &path, moment);
            service.assign_all_daily_mana_with_board(
                guild_id,
                &today_for_task,
                day_start_ts,
                &ash_fan_ids,
                white_stipend,
            )
        })
        .await
        .map_err(|error| format!("mana auto-assign blocking task failed: {error}"))?
        .map(Some)
        .map_err(|error| error.to_string())
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
            cold_cache_deferrals: Mutex::new(BTreeMap::new()),
        }
    }

    #[cfg(all(test, feature = "runtime-test-core"))]
    fn cold_cache_deferral_streak(&self, guild_id: i64) -> u32 {
        self.cold_cache_deferrals
            .lock()
            .expect("cold-cache deferral lock")
            .get(&guild_id)
            .copied()
            .unwrap_or(0)
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
