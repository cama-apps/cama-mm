//! Lifecycle-backed health state for the production container.
//!
//! A process check is not sufficient for a Discord bot: the process can still
//! exist while database initialization failed, the gateway is disconnected, or a
//! required worker crashed. The runtime therefore writes a small atomic state
//! file beside the SQLite database and refreshes it on a bounded heartbeat.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::time::MissedTickBehavior;

use crate::gateway::LifecycleEvent;

pub const DEFAULT_MAX_HEARTBEAT_AGE: Duration = Duration::from_secs(90);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const HEALTH_FORMAT_VERSION: u32 = 5;
const RUNTIME_NAME: &str = "rust";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Starting,
    Connecting,
    Ready,
    Reconnecting,
    Degraded,
    Stopping,
    Stopped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthSnapshot {
    pub format_version: u32,
    pub runtime: String,
    pub git_sha: String,
    pub pid: u32,
    pub database_path: PathBuf,
    pub status: HealthStatus,
    pub database_ready: bool,
    pub gateway_ready: bool,
    pub commands_registered: bool,
    pub command_tree_synchronized: bool,
    pub connected_shards: BTreeSet<u32>,
    pub applied_migrations: usize,
    pub required_migrations: usize,
    pub recovery_failure_count: usize,
    pub worker_registration_complete: bool,
    pub expected_workers: BTreeSet<String>,
    pub started_workers: BTreeSet<String>,
    pub failed_workers: BTreeSet<String>,
    pub lifecycle_gap_detected: bool,
    pub ready_at_unix_ms: Option<u64>,
    pub updated_at_unix_ms: u64,
    pub last_error: Option<String>,
}

impl HealthSnapshot {
    fn starting(database_path: PathBuf) -> Result<Self, HealthError> {
        Ok(Self {
            format_version: HEALTH_FORMAT_VERSION,
            runtime: RUNTIME_NAME.to_owned(),
            git_sha: deployment_revision(),
            pid: std::process::id(),
            database_path,
            status: HealthStatus::Starting,
            database_ready: false,
            gateway_ready: false,
            commands_registered: false,
            command_tree_synchronized: false,
            connected_shards: BTreeSet::new(),
            applied_migrations: 0,
            required_migrations: 0,
            recovery_failure_count: 0,
            worker_registration_complete: false,
            expected_workers: BTreeSet::new(),
            started_workers: BTreeSet::new(),
            failed_workers: BTreeSet::new(),
            lifecycle_gap_detected: false,
            ready_at_unix_ms: None,
            updated_at_unix_ms: unix_time_ms()?,
            last_error: None,
        })
    }

    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.format_version == HEALTH_FORMAT_VERSION
            && self.runtime == RUNTIME_NAME
            && !self.git_sha.trim().is_empty()
            && self.status == HealthStatus::Ready
            && self.database_ready
            && self.gateway_ready
            && self.commands_registered
            && self.command_tree_synchronized
            && self.required_migrations > 0
            && self.applied_migrations >= self.required_migrations
            && self.recovery_failure_count == 0
            && self.worker_registration_complete
            && self.expected_workers.is_subset(&self.started_workers)
            && self.failed_workers.is_empty()
            && !self.lifecycle_gap_detected
    }

    fn apply(&mut self, event: LifecycleEvent) -> Result<(), HealthError> {
        // Gateway and worker tasks shut down concurrently. Once termination
        // begins, a queued READY/shard-stage event must not resurrect the
        // marker or make normal worker completion look like a crash.
        if matches!(self.status, HealthStatus::Stopping | HealthStatus::Stopped)
            && !matches!(
                event,
                LifecycleEvent::ShutdownRequested
                    | LifecycleEvent::Stopped
                    | LifecycleEvent::BackgroundWorkerStopped { .. }
            )
        {
            self.updated_at_unix_ms = unix_time_ms()?;
            return Ok(());
        }
        match event {
            LifecycleEvent::Starting => {
                self.status = HealthStatus::Starting;
                self.gateway_ready = false;
                self.last_error = None;
            }
            LifecycleEvent::DatabaseReady {
                path,
                applied_migrations,
                required_migrations,
                ..
            } => {
                let path = absolute_path(&path)?;
                if path == self.database_path {
                    self.database_ready = true;
                    self.applied_migrations = applied_migrations;
                    self.required_migrations = required_migrations;
                } else {
                    self.database_ready = false;
                    self.status = HealthStatus::Degraded;
                    self.last_error = Some(bounded_error(format!(
                        "database initialization completed for {}, expected {}",
                        path.display(),
                        self.database_path.display()
                    )));
                }
            }
            LifecycleEvent::Connecting { .. } => {
                self.status = HealthStatus::Connecting;
                self.gateway_ready = false;
                self.commands_registered = false;
                self.command_tree_synchronized = false;
                self.connected_shards.clear();
                self.recovery_failure_count = 0;
                self.last_error = None;
            }
            LifecycleEvent::Ready { .. } => {
                self.gateway_ready = true;
                self.status = HealthStatus::Connecting;
            }
            LifecycleEvent::Resumed => {
                self.gateway_ready = true;
                self.status = HealthStatus::Degraded;
                self.mark_ready_if_possible()?;
            }
            LifecycleEvent::ReadyRecoveryCompleted {
                observer,
                failure_count,
                ..
            } if failure_count > 0 => {
                self.recovery_failure_count =
                    self.recovery_failure_count.saturating_add(failure_count);
                self.status = HealthStatus::Degraded;
                self.last_error = Some(bounded_error(format!(
                    "ready recovery {observer} reported {failure_count} failure(s)"
                )));
            }
            LifecycleEvent::Disconnected { reason } => {
                self.status = HealthStatus::Reconnecting;
                self.gateway_ready = false;
                self.connected_shards.clear();
                self.last_error = Some(bounded_error(reason));
            }
            LifecycleEvent::ReconnectScheduled { .. } => {
                self.status = HealthStatus::Reconnecting;
                self.gateway_ready = false;
                self.connected_shards.clear();
            }
            LifecycleEvent::GatewayShardStageChanged {
                shard_id,
                connected,
                stage,
            } => {
                if connected {
                    self.connected_shards.insert(shard_id);
                } else {
                    self.connected_shards.remove(&shard_id);
                    self.gateway_ready = false;
                    self.status = HealthStatus::Reconnecting;
                    self.last_error = Some(bounded_error(format!(
                        "Discord shard {shard_id} entered {stage}"
                    )));
                }
            }
            LifecycleEvent::BackgroundWorkerFailed { name, error } => {
                self.started_workers.remove(&name);
                self.failed_workers.insert(name);
                self.status = HealthStatus::Degraded;
                self.last_error = Some(bounded_error(error));
            }
            LifecycleEvent::ShutdownRequested => {
                self.status = HealthStatus::Stopping;
                self.gateway_ready = false;
            }
            LifecycleEvent::Stopped => {
                self.status = HealthStatus::Stopped;
                self.gateway_ready = false;
            }
            LifecycleEvent::CommandsRegistered { synchronized, .. } => {
                self.commands_registered = true;
                self.command_tree_synchronized = synchronized;
                if !synchronized {
                    self.status = HealthStatus::Degraded;
                    self.last_error =
                        Some("Discord global command tree was not synchronized".to_owned());
                } else {
                    self.mark_ready_if_possible()?;
                }
            }
            LifecycleEvent::BackgroundWorkersRegistered { names } => {
                self.worker_registration_complete = true;
                self.expected_workers = names.into_iter().collect();
                self.started_workers
                    .retain(|name| self.expected_workers.contains(name));
                self.failed_workers
                    .retain(|name| self.expected_workers.contains(name));
                self.mark_ready_if_possible()?;
            }
            LifecycleEvent::BackgroundWorkerStarting { name, .. } => {
                self.failed_workers.remove(&name);
                self.started_workers.insert(name);
                self.mark_ready_if_possible()?;
            }
            LifecycleEvent::BackgroundWorkerStopped { name }
                if !matches!(self.status, HealthStatus::Stopping | HealthStatus::Stopped) =>
            {
                self.started_workers.remove(&name);
                self.failed_workers.insert(name.clone());
                self.status = HealthStatus::Degraded;
                self.last_error = Some(bounded_error(format!(
                    "required background worker {name} stopped unexpectedly"
                )));
            }
            LifecycleEvent::ReadyRecoveryCompleted { .. }
            | LifecycleEvent::BackgroundWorkerRestartScheduled { .. }
            | LifecycleEvent::BackgroundWorkerStopped { .. } => {}
        }
        self.enforce_lifecycle_gap();
        self.updated_at_unix_ms = unix_time_ms()?;
        Ok(())
    }

    fn touch(&mut self) -> Result<(), HealthError> {
        self.updated_at_unix_ms = unix_time_ms()?;
        Ok(())
    }

    fn mark_ready_if_possible(&mut self) -> Result<(), HealthError> {
        if self.database_ready
            && self.gateway_ready
            && self.commands_registered
            && self.command_tree_synchronized
            && self.required_migrations > 0
            && self.applied_migrations >= self.required_migrations
            && self.recovery_failure_count == 0
            && self.worker_registration_complete
            && self.expected_workers.is_subset(&self.started_workers)
            && self.failed_workers.is_empty()
            && !self.lifecycle_gap_detected
        {
            self.status = HealthStatus::Ready;
            self.ready_at_unix_ms = Some(unix_time_ms()?);
            self.last_error = None;
        }
        Ok(())
    }

    fn mark_lifecycle_gap(&mut self, skipped: u64) -> Result<(), HealthError> {
        self.lifecycle_gap_detected = true;
        self.status = HealthStatus::Degraded;
        self.gateway_ready = false;
        self.last_error = Some(format!(
            "health reporter missed {skipped} lifecycle event(s)"
        ));
        self.touch()
    }

    fn enforce_lifecycle_gap(&mut self) {
        if self.lifecycle_gap_detected
            && !matches!(self.status, HealthStatus::Stopping | HealthStatus::Stopped)
        {
            self.status = HealthStatus::Degraded;
            if self.last_error.is_none() {
                self.last_error =
                    Some("health reporter previously missed lifecycle events".to_owned());
            }
        }
    }
}

#[derive(Debug)]
pub struct HealthReporter {
    path: PathBuf,
    snapshot: HealthSnapshot,
}

impl HealthReporter {
    /// Invalidate any stale ready marker immediately after the process lock is
    /// acquired and before migration or provider construction begins.
    pub fn initialize(database_path: &Path) -> Result<Self, HealthError> {
        let database_path = absolute_path(database_path)?;
        let reporter = Self {
            path: health_path(&database_path),
            snapshot: HealthSnapshot::starting(database_path)?,
        };
        reporter.persist()?;
        Ok(reporter)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn run(
        mut self,
        mut events: broadcast::Receiver<LifecycleEvent>,
    ) -> Result<(), HealthError> {
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // Consume the interval's immediate first tick. `initialize` already
        // persisted the starting state synchronously.
        heartbeat.tick().await;
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(event) => {
                        let stopped = event == LifecycleEvent::Stopped;
                        self.snapshot.apply(event)?;
                        self.persist()?;
                        if stopped {
                            return Ok(());
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        self.snapshot.mark_lifecycle_gap(skipped)?;
                        self.persist()?;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        self.snapshot.status = HealthStatus::Stopped;
                        self.snapshot.gateway_ready = false;
                        self.snapshot.last_error = Some(
                            "runtime lifecycle stream closed before a stopped event".to_owned(),
                        );
                        self.snapshot.touch()?;
                        self.persist()?;
                        return Ok(());
                    }
                },
                _ = heartbeat.tick() => {
                    self.snapshot.touch()?;
                    self.persist()?;
                }
            }
        }
    }

    fn persist(&self) -> Result<(), HealthError> {
        persist_snapshot(&self.path, &self.snapshot)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthCheckReport {
    pub snapshot: HealthSnapshot,
    pub heartbeat_age: Duration,
}

pub fn check_health(
    database_path: &Path,
    maximum_age: Duration,
) -> Result<HealthCheckReport, HealthError> {
    check_health_for_deployment(
        database_path,
        maximum_age,
        RUNTIME_NAME,
        &deployment_revision(),
    )
}

fn check_health_for_deployment(
    database_path: &Path,
    maximum_age: Duration,
    expected_runtime: &str,
    expected_revision: &str,
) -> Result<HealthCheckReport, HealthError> {
    let database_path = absolute_path(database_path)?;
    let path = health_path(&database_path);
    let bytes = fs::read(&path).map_err(|source| HealthError::Read { path, source })?;
    let snapshot: HealthSnapshot =
        serde_json::from_slice(&bytes).map_err(HealthError::InvalidState)?;
    if snapshot.database_path != database_path {
        return Err(HealthError::DatabaseMismatch {
            expected: database_path,
            actual: snapshot.database_path,
        });
    }
    if snapshot.runtime != expected_runtime || snapshot.git_sha != expected_revision {
        return Err(HealthError::DeploymentMismatch {
            expected_runtime: expected_runtime.to_owned(),
            actual_runtime: snapshot.runtime,
            expected_revision: expected_revision.to_owned(),
            actual_revision: snapshot.git_sha,
        });
    }
    if !snapshot.is_healthy() {
        return Err(HealthError::Unhealthy {
            status: snapshot.status,
            detail: snapshot
                .last_error
                .clone()
                .unwrap_or_else(|| "runtime has not reached a healthy ready state".to_owned()),
        });
    }
    let now = unix_time_ms()?;
    let age_ms =
        now.checked_sub(snapshot.updated_at_unix_ms)
            .ok_or(HealthError::FutureHeartbeat {
                heartbeat: snapshot.updated_at_unix_ms,
                now,
            })?;
    let heartbeat_age = Duration::from_millis(age_ms);
    if heartbeat_age > maximum_age {
        return Err(HealthError::StaleHeartbeat {
            age: heartbeat_age,
            maximum: maximum_age,
        });
    }
    verify_database_ledger(&database_path)?;

    #[cfg(target_os = "linux")]
    if !Path::new("/proc").join(snapshot.pid.to_string()).exists() {
        return Err(HealthError::MissingProcess(snapshot.pid));
    }

    Ok(HealthCheckReport {
        snapshot,
        heartbeat_age,
    })
}

fn deployment_revision() -> String {
    std::env::var("GIT_SHA")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[must_use]
pub fn health_path(database_path: &Path) -> PathBuf {
    let mut file_name = database_path
        .file_name()
        .map_or_else(|| "cama.db".into(), std::ffi::OsString::from);
    file_name.push(".rust-health.json");
    database_path.with_file_name(file_name)
}

fn verify_database_ledger(path: &Path) -> Result<(), HealthError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(HealthError::Database)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(HealthError::Database)?;
    let mut statement = connection
        .prepare("SELECT name FROM schema_migrations")
        .map_err(HealthError::Database)?;
    let applied = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(HealthError::Database)?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(HealthError::Database)?;
    let missing = cama_db::expected_migrations()
        .into_iter()
        .filter(|migration| !applied.contains(*migration))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(HealthError::MigrationLedger { missing });
    }
    let quick_check: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(HealthError::Database)?;
    if quick_check != "ok" {
        return Err(HealthError::IntegrityCheck(quick_check));
    }
    Ok(())
}

fn persist_snapshot(path: &Path, snapshot: &HealthSnapshot) -> Result<(), HealthError> {
    let mut bytes = serde_json::to_vec(snapshot).map_err(HealthError::Serialize)?;
    bytes.push(b'\n');
    let temporary = path.with_extension(format!("health.tmp.{}", snapshot.pid));
    let result = (|| -> Result<(), HealthError> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| HealthError::Write {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| HealthError::Write {
                path: temporary.clone(),
                source,
            })?;
        fs::rename(&temporary, path).map_err(|source| HealthError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn absolute_path(path: &Path) -> Result<PathBuf, HealthError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(HealthError::CurrentDirectory)
    }
}

fn unix_time_ms() -> Result<u64, HealthError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(HealthError::Clock)?;
    u64::try_from(duration.as_millis()).map_err(|_| HealthError::ClockOverflow)
}

fn bounded_error(mut error: String) -> String {
    const MAX_CHARS: usize = 512;
    if error.chars().count() > MAX_CHARS {
        error = error.chars().take(MAX_CHARS).collect();
    }
    error
}

#[derive(Debug, Error)]
pub enum HealthError {
    #[error("could not resolve current directory: {0}")]
    CurrentDirectory(std::io::Error),
    #[error("system clock is before the Unix epoch: {0}")]
    Clock(std::time::SystemTimeError),
    #[error("system clock millisecond value does not fit in u64")]
    ClockOverflow,
    #[error("could not read health state {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not write health state {}: {source}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not serialize health state: {0}")]
    Serialize(serde_json::Error),
    #[error("health state is not valid JSON: {0}")]
    InvalidState(serde_json::Error),
    #[error("health state belongs to {actual:?}, expected {expected:?}")]
    DatabaseMismatch { expected: PathBuf, actual: PathBuf },
    #[error(
        "health state belongs to runtime {actual_runtime:?} revision {actual_revision:?}, expected runtime {expected_runtime:?} revision {expected_revision:?}"
    )]
    DeploymentMismatch {
        expected_runtime: String,
        actual_runtime: String,
        expected_revision: String,
        actual_revision: String,
    },
    #[error("runtime health is {status:?}: {detail}")]
    Unhealthy {
        status: HealthStatus,
        detail: String,
    },
    #[error("health heartbeat is from the future ({heartbeat} > {now})")]
    FutureHeartbeat { heartbeat: u64, now: u64 },
    #[error("health heartbeat is stale ({age:?} > {maximum:?})")]
    StaleHeartbeat { age: Duration, maximum: Duration },
    #[error("runtime process {0} no longer exists")]
    MissingProcess(u32),
    #[error("health database probe failed: {0}")]
    Database(rusqlite::Error),
    #[error("migration ledger is missing required migrations: {}", .missing.join(", "))]
    MigrationLedger { missing: Vec<String> },
    #[error("SQLite quick_check failed: {0}")]
    IntegrityCheck(String),
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn database_initialized(snapshot: &mut HealthSnapshot) {
        snapshot
            .apply(LifecycleEvent::DatabaseReady {
                path: snapshot.database_path.clone(),
                applied_migrations: 2,
                required_migrations: 2,
                newly_applied_migrations: 2,
                created_tables: 2,
                rebuilt_tables: 0,
            })
            .expect("database event");
    }

    fn synchronize_commands(snapshot: &mut HealthSnapshot) {
        snapshot
            .apply(LifecycleEvent::CommandsRegistered {
                command_count: 2,
                component_route_count: 1,
                synchronized: true,
            })
            .expect("command registration event");
    }

    fn register_workers(snapshot: &mut HealthSnapshot, names: &[&str]) {
        snapshot
            .apply(LifecycleEvent::BackgroundWorkersRegistered {
                names: names.iter().map(|name| (*name).to_owned()).collect(),
            })
            .expect("worker registration event");
    }

    fn create_ledger(path: &Path) {
        let connection = Connection::open(path).expect("health fixture database");
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (name TEXT PRIMARY KEY, applied_at TEXT);",
            )
            .expect("migration ledger");
        for migration in cama_db::expected_migrations() {
            connection
                .execute(
                    "INSERT INTO schema_migrations (name, applied_at) VALUES (?1, 'now')",
                    [migration],
                )
                .expect("migration row");
        }
    }

    #[test]
    fn only_initialized_ready_runtime_is_healthy() {
        let directory = tempdir().expect("temporary directory");
        let mut snapshot =
            HealthSnapshot::starting(directory.path().join("cama.db")).expect("starting state");
        snapshot
            .apply(LifecycleEvent::Connecting { attempt: 1 })
            .expect("connecting event");
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 2,
            })
            .expect("ready event");
        assert!(!snapshot.is_healthy());

        database_initialized(&mut snapshot);
        synchronize_commands(&mut snapshot);
        assert!(!snapshot.is_healthy());
        register_workers(&mut snapshot, &[]);
        assert!(snapshot.is_healthy());
    }

    #[test]
    fn zero_migration_contract_and_mismatched_initialization_are_rejected() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("cama.db");
        let mut snapshot = HealthSnapshot::starting(database.clone()).expect("starting state");
        snapshot
            .apply(LifecycleEvent::DatabaseReady {
                path: database.clone(),
                applied_migrations: 0,
                required_migrations: 0,
                newly_applied_migrations: 0,
                created_tables: 0,
                rebuilt_tables: 0,
            })
            .expect("empty database event");
        register_workers(&mut snapshot, &[]);
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 1,
            })
            .expect("ready event");
        synchronize_commands(&mut snapshot);
        assert!(!snapshot.is_healthy());

        snapshot
            .apply(LifecycleEvent::DatabaseReady {
                path: directory.path().join("other.db"),
                applied_migrations: 2,
                required_migrations: 2,
                newly_applied_migrations: 2,
                created_tables: 2,
                rebuilt_tables: 0,
            })
            .expect("mismatched database event");
        assert!(!snapshot.database_ready);
        assert_eq!(snapshot.status, HealthStatus::Degraded);
        assert!(
            snapshot
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("other.db"))
        );
    }

    #[test]
    fn disconnect_recovery_failure_and_worker_failure_are_fail_closed() {
        let directory = tempdir().expect("temporary directory");
        let mut snapshot =
            HealthSnapshot::starting(directory.path().join("cama.db")).expect("starting state");
        database_initialized(&mut snapshot);
        register_workers(&mut snapshot, &[]);
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 2,
            })
            .expect("ready event");
        synchronize_commands(&mut snapshot);
        assert!(snapshot.is_healthy());

        snapshot
            .apply(LifecycleEvent::Disconnected {
                reason: "gateway lost".to_owned(),
            })
            .expect("disconnect event");
        assert!(!snapshot.is_healthy());

        snapshot
            .apply(LifecycleEvent::Connecting { attempt: 2 })
            .expect("connecting event");
        snapshot
            .apply(LifecycleEvent::ReadyRecoveryCompleted {
                observer: "lobbies".to_owned(),
                guilds_attempted: 1,
                guilds_refreshed: 0,
                guilds_superseded: 0,
                members_refreshed: 0,
                failure_count: 1,
            })
            .expect("recovery event");
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 1,
            })
            .expect("ready event");
        synchronize_commands(&mut snapshot);
        assert!(!snapshot.is_healthy());

        snapshot
            .apply(LifecycleEvent::BackgroundWorkerFailed {
                name: "prediction refresh".to_owned(),
                error: "boom".to_owned(),
            })
            .expect("worker failure");
        assert!(snapshot.failed_workers.contains("prediction refresh"));
        assert!(!snapshot.is_healthy());
    }

    #[test]
    fn internal_shard_disconnect_invalidates_health_until_resume() {
        let directory = tempdir().expect("temporary directory");
        let mut snapshot =
            HealthSnapshot::starting(directory.path().join("cama.db")).expect("starting state");
        database_initialized(&mut snapshot);
        register_workers(&mut snapshot, &[]);
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 1,
            })
            .expect("ready event");
        synchronize_commands(&mut snapshot);
        assert!(snapshot.is_healthy());

        snapshot
            .apply(LifecycleEvent::GatewayShardStageChanged {
                shard_id: 0,
                connected: false,
                stage: "resuming".to_owned(),
            })
            .expect("shard stage event");
        assert!(!snapshot.gateway_ready);
        assert!(!snapshot.is_healthy());

        snapshot
            .apply(LifecycleEvent::GatewayShardStageChanged {
                shard_id: 0,
                connected: true,
                stage: "connected".to_owned(),
            })
            .expect("connected stage event");
        assert!(!snapshot.is_healthy());
        snapshot
            .apply(LifecycleEvent::Resumed)
            .expect("resume event");
        assert!(snapshot.is_healthy());
    }

    #[test]
    fn lifecycle_gap_cannot_be_healed_by_later_ready_events() {
        let directory = tempdir().expect("temporary directory");
        let mut snapshot =
            HealthSnapshot::starting(directory.path().join("cama.db")).expect("starting state");
        database_initialized(&mut snapshot);
        register_workers(&mut snapshot, &[]);
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 1,
            })
            .expect("ready event");
        synchronize_commands(&mut snapshot);
        assert!(snapshot.is_healthy());

        snapshot.mark_lifecycle_gap(3).expect("lifecycle gap");
        snapshot
            .apply(LifecycleEvent::Resumed)
            .expect("resumed event");
        synchronize_commands(&mut snapshot);

        assert!(snapshot.lifecycle_gap_detected);
        assert_eq!(snapshot.status, HealthStatus::Degraded);
        assert!(!snapshot.is_healthy());
    }

    #[test]
    fn unexpected_worker_completion_is_unhealthy_but_shutdown_completion_is_not_a_failure() {
        let directory = tempdir().expect("temporary directory");
        let mut snapshot =
            HealthSnapshot::starting(directory.path().join("cama.db")).expect("starting state");
        database_initialized(&mut snapshot);
        register_workers(&mut snapshot, &["prediction refresh", "reminders"]);
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 1,
            })
            .expect("ready event");
        synchronize_commands(&mut snapshot);
        assert!(!snapshot.is_healthy());
        snapshot
            .apply(LifecycleEvent::BackgroundWorkerStarting {
                name: "prediction refresh".to_owned(),
                attempt: 1,
            })
            .expect("worker starting event");
        assert!(!snapshot.is_healthy());
        snapshot
            .apply(LifecycleEvent::BackgroundWorkerStarting {
                name: "reminders".to_owned(),
                attempt: 1,
            })
            .expect("second worker starting event");
        assert!(snapshot.is_healthy());

        snapshot
            .apply(LifecycleEvent::BackgroundWorkerFailed {
                name: "prediction refresh".to_owned(),
                error: "provider timeout".to_owned(),
            })
            .expect("worker failed event");
        assert!(!snapshot.is_healthy());
        snapshot
            .apply(LifecycleEvent::BackgroundWorkerStarting {
                name: "prediction refresh".to_owned(),
                attempt: 2,
            })
            .expect("worker restarted event");
        assert!(snapshot.is_healthy());

        snapshot
            .apply(LifecycleEvent::BackgroundWorkerStopped {
                name: "prediction refresh".to_owned(),
            })
            .expect("worker stopped event");
        assert!(snapshot.failed_workers.contains("prediction refresh"));
        assert!(!snapshot.is_healthy());

        let mut stopping =
            HealthSnapshot::starting(directory.path().join("shutdown.db")).expect("starting state");
        stopping
            .apply(LifecycleEvent::ShutdownRequested)
            .expect("shutdown event");
        stopping
            .apply(LifecycleEvent::GatewayShardStageChanged {
                shard_id: 0,
                connected: false,
                stage: "disconnected".to_owned(),
            })
            .expect("late shard event");
        stopping
            .apply(LifecycleEvent::Resumed)
            .expect("late resume event");
        stopping
            .apply(LifecycleEvent::BackgroundWorkerStopped {
                name: "prediction refresh".to_owned(),
            })
            .expect("worker stopped during shutdown");
        assert!(stopping.failed_workers.is_empty());
        assert_eq!(stopping.status, HealthStatus::Stopping);
    }

    #[test]
    fn persisted_ready_state_passes_fresh_ledger_probe() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("cama.db");
        create_ledger(&database);
        let reporter = HealthReporter::initialize(&database).expect("health reporter");
        let mut snapshot = reporter.snapshot.clone();
        database_initialized(&mut snapshot);
        register_workers(&mut snapshot, &[]);
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 1,
            })
            .expect("ready event");
        synchronize_commands(&mut snapshot);
        persist_snapshot(reporter.path(), &snapshot).expect("persist ready state");

        let report = check_health(&database, Duration::from_secs(1)).expect("healthy runtime");
        assert!(report.snapshot.is_healthy());
        assert!(report.heartbeat_age <= Duration::from_secs(1));
    }

    #[test]
    fn ledger_probe_rejects_a_same_size_ledger_missing_a_required_migration() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("cama.db");
        create_ledger(&database);
        let missing = cama_db::expected_migrations()[0];
        let connection = Connection::open(&database).expect("health fixture database");
        connection
            .execute("DELETE FROM schema_migrations WHERE name = ?1", [missing])
            .expect("remove required migration");
        connection
            .execute(
                "INSERT INTO schema_migrations (name, applied_at) VALUES ('retired_historical_migration', 'now')",
                [],
            )
            .expect("insert historical replacement");
        let row_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("migration row count");
        assert_eq!(
            usize::try_from(row_count).expect("valid migration row count"),
            cama_db::expected_migrations().len()
        );
        drop(connection);

        assert!(matches!(
            verify_database_ledger(&database),
            Err(HealthError::MigrationLedger { missing: actual })
                if actual == [missing]
        ));
    }

    #[test]
    fn ledger_probe_tolerates_historical_extra_migrations() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("cama.db");
        create_ledger(&database);
        let connection = Connection::open(&database).expect("health fixture database");
        connection
            .execute(
                "INSERT INTO schema_migrations (name, applied_at) VALUES ('retired_historical_migration', 'now')",
                [],
            )
            .expect("insert historical migration");
        drop(connection);

        verify_database_ledger(&database).expect("required migration ledger");
    }

    #[test]
    fn health_probe_rejects_a_marker_from_another_runtime_or_revision() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("cama.db");
        create_ledger(&database);
        let reporter = HealthReporter::initialize(&database).expect("health reporter");
        let mut snapshot = reporter.snapshot.clone();
        database_initialized(&mut snapshot);
        register_workers(&mut snapshot, &[]);
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 1,
            })
            .expect("ready event");
        synchronize_commands(&mut snapshot);
        persist_snapshot(reporter.path(), &snapshot).expect("persist ready state");

        assert!(matches!(
            check_health_for_deployment(
                &database,
                Duration::from_secs(1),
                "python",
                &snapshot.git_sha,
            ),
            Err(HealthError::DeploymentMismatch {
                expected_runtime,
                actual_runtime,
                ..
            }) if expected_runtime == "python" && actual_runtime == "rust"
        ));
        assert!(matches!(
            check_health_for_deployment(
                &database,
                Duration::from_secs(1),
                "rust",
                "different-revision",
            ),
            Err(HealthError::DeploymentMismatch {
                expected_revision,
                actual_revision,
                ..
            }) if expected_revision == "different-revision" && actual_revision == snapshot.git_sha
        ));
    }

    #[test]
    fn stale_and_stopped_states_are_rejected() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("cama.db");
        create_ledger(&database);
        let reporter = HealthReporter::initialize(&database).expect("health reporter");
        let mut snapshot = reporter.snapshot.clone();
        database_initialized(&mut snapshot);
        register_workers(&mut snapshot, &[]);
        snapshot
            .apply(LifecycleEvent::Ready {
                bot_user_id: 7,
                guild_count: 1,
            })
            .expect("ready event");
        synchronize_commands(&mut snapshot);
        snapshot.updated_at_unix_ms = snapshot.updated_at_unix_ms.saturating_sub(5_000);
        persist_snapshot(reporter.path(), &snapshot).expect("persist stale state");
        assert!(matches!(
            check_health(&database, Duration::from_secs(1)),
            Err(HealthError::StaleHeartbeat { .. })
        ));

        snapshot
            .apply(LifecycleEvent::Stopped)
            .expect("stopped event");
        persist_snapshot(reporter.path(), &snapshot).expect("persist stopped state");
        assert!(matches!(
            check_health(&database, Duration::from_secs(10)),
            Err(HealthError::Unhealthy {
                status: HealthStatus::Stopped,
                ..
            })
        ));
    }

    #[test]
    fn health_path_is_database_specific() {
        assert_eq!(
            health_path(Path::new("/app/data/cama_shuffle.db")),
            Path::new("/app/data/cama_shuffle.db.rust-health.json")
        );
    }
}
