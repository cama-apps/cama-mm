use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_db::{
    audit_database,
    schema_manager::{MigrationSettings, initialize_or_migrate_with_settings},
};
use thiserror::Error;
use tokio::sync::{broadcast, watch};

use crate::config::{DiscordToken, RuntimeConfig};
use crate::gateway_events::GatewayEventObservers;
use crate::global_hooks::GlobalInteractionHooks;
use crate::raw_reactions::RawReactionObservers;
use crate::registration::Registry;
use crate::worker::{BackgroundWorkerSpec, WorkerContext};

/// Python uses `discord.Intents.default()` plus all three privileged intents.
/// The resulting profile is the complete Discord intent set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewayIntentProfile {
    pub guilds: bool,
    pub guild_members: bool,
    pub guild_presences: bool,
    pub guild_messages: bool,
    pub guild_message_reactions: bool,
    pub direct_messages: bool,
    pub message_content: bool,
}

impl GatewayIntentProfile {
    #[must_use]
    pub const fn python_parity() -> Self {
        Self {
            guilds: true,
            guild_members: true,
            guild_presences: true,
            guild_messages: true,
            guild_message_reactions: true,
            direct_messages: true,
            message_content: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleEvent {
    Starting,
    DatabaseReady {
        path: PathBuf,
        applied_migrations: usize,
        required_migrations: usize,
        newly_applied_migrations: usize,
        created_tables: usize,
        rebuilt_tables: usize,
        historical_extra_migrations: usize,
    },
    Connecting {
        attempt: u32,
    },
    Ready {
        bot_user_id: u64,
        guild_count: usize,
    },
    CommandsRegistered {
        command_count: usize,
        component_route_count: usize,
        synchronized: bool,
    },
    GatewayShardStageChanged {
        shard_id: u32,
        connected: bool,
        stage: String,
    },
    Resumed,
    ReadyRecoveryCompleted {
        observer: String,
        guilds_attempted: usize,
        guilds_refreshed: usize,
        guilds_superseded: usize,
        members_refreshed: usize,
        failure_count: usize,
    },
    Disconnected {
        reason: String,
    },
    ReconnectScheduled {
        attempt: u32,
        delay: Duration,
    },
    BackgroundWorkerStarting {
        name: String,
        attempt: u32,
    },
    BackgroundWorkersRegistered {
        names: Vec<String>,
    },
    BackgroundWorkerFailed {
        name: String,
        error: String,
    },
    BackgroundWorkerRestartScheduled {
        name: String,
        attempt: u32,
        delay: Duration,
    },
    BackgroundWorkerStopped {
        name: String,
    },
    ShutdownRequested,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseAdmissionReport {
    pub path: PathBuf,
    pub applied_migrations: usize,
    pub required_migrations: usize,
    pub newly_applied_migrations: usize,
    pub created_tables: usize,
    pub rebuilt_tables: usize,
    pub historical_extra_migrations: usize,
}

#[async_trait]
pub trait DatabaseAdmission: Send + Sync {
    async fn admit(&self, path: &Path) -> Result<DatabaseAdmissionReport, String>;
}

#[derive(Clone, Debug, Default)]
pub struct SqliteDatabaseAdmission {
    migration_settings: MigrationSettings,
}

impl SqliteDatabaseAdmission {
    #[must_use]
    pub const fn with_migration_settings(migration_settings: MigrationSettings) -> Self {
        Self { migration_settings }
    }

    #[must_use]
    pub const fn migration_settings(&self) -> &MigrationSettings {
        &self.migration_settings
    }
}

#[async_trait]
impl DatabaseAdmission for SqliteDatabaseAdmission {
    async fn admit(&self, path: &Path) -> Result<DatabaseAdmissionReport, String> {
        let path = path.to_path_buf();
        let settings = self.migration_settings.clone();
        tokio::task::spawn_blocking(move || -> Result<DatabaseAdmissionReport, String> {
            let migration = initialize_or_migrate_with_settings(&path, &settings)
                .map_err(|error| error.to_string())?;
            let audit = audit_database(&path).map_err(|error| error.to_string())?;
            if !audit.is_compatible() {
                return Err(audit.issues().join("; "));
            }
            Ok(DatabaseAdmissionReport {
                path: audit.path,
                applied_migrations: audit.applied_migration_count,
                required_migrations: audit.required_migration_count,
                newly_applied_migrations: migration.newly_applied.len(),
                created_tables: migration.created_tables.len(),
                rebuilt_tables: migration.rebuilt_tables.len(),
                historical_extra_migrations: migration.historical_extra_migrations.len(),
            })
        })
        .await
        .map_err(|error| format!("database admission task failed: {error}"))?
    }
}

/// Proof that schema migration and compatibility audit completed before the
/// production service graph was constructed.
#[derive(Clone, Debug)]
pub struct CompletedDatabaseAdmission {
    report: DatabaseAdmissionReport,
}

impl CompletedDatabaseAdmission {
    #[must_use]
    pub const fn new(report: DatabaseAdmissionReport) -> Self {
        Self { report }
    }
}

#[async_trait]
impl DatabaseAdmission for CompletedDatabaseAdmission {
    async fn admit(&self, path: &Path) -> Result<DatabaseAdmissionReport, String> {
        if path != self.report.path {
            return Err(format!(
                "completed database admission is for {}, not {}",
                self.report.path.display(),
                path.display()
            ));
        }
        Ok(self.report.clone())
    }
}

#[derive(Clone)]
pub struct GatewaySession {
    pub token: Arc<DiscordToken>,
    pub intents: GatewayIntentProfile,
    pub registry: Arc<Registry>,
    pub observers: GatewayEventObservers,
    pub global_interaction_hooks: Option<GlobalInteractionHooks>,
    pub raw_reaction_observers: RawReactionObservers,
    pub events: broadcast::Sender<LifecycleEvent>,
    pub shutdown: watch::Receiver<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GatewaySessionEnd {
    Shutdown,
    Reconnect { reason: String },
}

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("fatal gateway setup failure: {0}")]
    Fatal(String),
    #[error("recoverable gateway failure: {0}")]
    Recoverable(String),
}

#[async_trait]
pub trait GatewayTransport: Send {
    async fn run_session(
        &mut self,
        session: GatewaySession,
    ) -> Result<GatewaySessionEnd, GatewayError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectPolicy {
    initial: Duration,
    maximum: Duration,
}

impl ReconnectPolicy {
    #[must_use]
    pub const fn new(initial: Duration, maximum: Duration) -> Self {
        Self { initial, maximum }
    }

    #[must_use]
    pub fn delay(self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(31);
        self.initial
            .checked_mul(1_u32 << exponent)
            .unwrap_or(self.maximum)
            .min(self.maximum)
    }
}

pub struct Runtime<G, D> {
    config: RuntimeConfig,
    registry: Arc<Registry>,
    gateway: G,
    database: D,
    reconnect: ReconnectPolicy,
    events: broadcast::Sender<LifecycleEvent>,
    workers: Vec<BackgroundWorkerSpec>,
    observers: GatewayEventObservers,
    global_interaction_hooks: Option<GlobalInteractionHooks>,
    raw_reaction_observers: RawReactionObservers,
}

impl<G, D> Runtime<G, D>
where
    G: GatewayTransport,
    D: DatabaseAdmission,
{
    #[must_use]
    pub fn new(config: RuntimeConfig, registry: Registry, gateway: G, database: D) -> Self {
        let reconnect = ReconnectPolicy::new(config.reconnect_initial, config.reconnect_max);
        let (events, _) = broadcast::channel(128);
        Self {
            config,
            registry: Arc::new(registry),
            gateway,
            database,
            reconnect,
            events,
            workers: Vec::new(),
            observers: GatewayEventObservers::default(),
            global_interaction_hooks: None,
            raw_reaction_observers: RawReactionObservers::default(),
        }
    }

    #[must_use]
    pub fn with_worker(mut self, worker: BackgroundWorkerSpec) -> Self {
        self.workers.push(worker);
        self
    }

    /// Attach production gateway observers after the admitted service graph
    /// has been constructed and before the first Discord connection.
    #[must_use]
    pub fn with_gateway_event_observers(mut self, observers: GatewayEventObservers) -> Self {
        self.observers = observers;
        self
    }

    /// Attach the process-wide tree-error and command-usage policy before the
    /// first Discord session is constructed.
    #[must_use]
    pub fn with_global_interaction_hooks(mut self, hooks: GlobalInteractionHooks) -> Self {
        self.global_interaction_hooks = Some(hooks);
        self
    }

    /// Attach typed raw-reaction consumers. An empty fan-out does not claim
    /// that any Python reaction behavior has been ported.
    #[must_use]
    pub fn with_raw_reaction_observers(mut self, observers: RawReactionObservers) -> Self {
        self.raw_reaction_observers = observers;
        self
    }

    #[must_use]
    pub const fn events(&self) -> &broadcast::Sender<LifecycleEvent> {
        &self.events
    }

    pub async fn run_until<F>(mut self, shutdown: F) -> Result<(), RuntimeError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        emit(&self.events, LifecycleEvent::Starting);
        let admission = match self.database.admit(&self.config.db_path).await {
            Ok(admission) => admission,
            Err(error) => {
                emit(&self.events, LifecycleEvent::Stopped);
                return Err(RuntimeError::DatabaseAdmission(error));
            }
        };
        emit(
            &self.events,
            LifecycleEvent::DatabaseReady {
                path: admission.path,
                applied_migrations: admission.applied_migrations,
                required_migrations: admission.required_migrations,
                newly_applied_migrations: admission.newly_applied_migrations,
                created_tables: admission.created_tables,
                rebuilt_tables: admission.rebuilt_tables,
                historical_extra_migrations: admission.historical_extra_migrations,
            },
        );

        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let events = self.events.clone();
        let shutdown_for_signal = shutdown_sender.clone();
        let signal_task = tokio::spawn(async move {
            shutdown.await;
            emit(&events, LifecycleEvent::ShutdownRequested);
            let _ = shutdown_for_signal.send(true);
        });
        let mut worker_names = self
            .workers
            .iter()
            .map(|worker| worker.name.clone())
            .collect::<Vec<_>>();
        worker_names.sort();
        worker_names.dedup();
        emit(
            &self.events,
            LifecycleEvent::BackgroundWorkersRegistered {
                names: worker_names,
            },
        );
        let worker_tasks = self
            .workers
            .into_iter()
            .map(|worker| {
                let ready_events = self.events.subscribe();
                let events = self.events.clone();
                let shutdown = shutdown_receiver.clone();
                tokio::spawn(supervise_worker(worker, ready_events, shutdown, events))
            })
            .collect::<Vec<_>>();

        let mut attempt = 1_u32;
        let mut terminal_error = None;
        loop {
            if *shutdown_receiver.borrow() {
                break;
            }
            emit(&self.events, LifecycleEvent::Connecting { attempt });
            let session = GatewaySession {
                token: Arc::new(self.config.token.clone()),
                intents: GatewayIntentProfile::python_parity(),
                registry: Arc::clone(&self.registry),
                observers: self.observers.clone(),
                global_interaction_hooks: self.global_interaction_hooks.clone(),
                raw_reaction_observers: self.raw_reaction_observers.clone(),
                events: self.events.clone(),
                shutdown: shutdown_receiver.clone(),
            };

            let reconnect_reason = match self.gateway.run_session(session).await {
                Ok(GatewaySessionEnd::Shutdown) => break,
                Ok(GatewaySessionEnd::Reconnect { reason }) => reason,
                Err(GatewayError::Recoverable(reason)) => reason,
                Err(GatewayError::Fatal(reason)) => {
                    terminal_error = Some(RuntimeError::Gateway(reason));
                    break;
                }
            };

            emit(
                &self.events,
                LifecycleEvent::Disconnected {
                    reason: reconnect_reason,
                },
            );
            let delay = self.reconnect.delay(attempt);
            emit(
                &self.events,
                LifecycleEvent::ReconnectScheduled { attempt, delay },
            );

            let mut reconnect_shutdown = shutdown_receiver.clone();
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                result = reconnect_shutdown.changed() => {
                    if result.is_err() || *reconnect_shutdown.borrow() {
                        break;
                    }
                }
            }
            attempt = attempt.saturating_add(1);
        }

        signal_task.abort();
        let _ = shutdown_sender.send(true);
        for mut task in worker_tasks {
            if tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
        emit(&self.events, LifecycleEvent::Stopped);
        terminal_error.map_or(Ok(()), Err)
    }
}

async fn supervise_worker(
    spec: BackgroundWorkerSpec,
    mut ready_events: broadcast::Receiver<LifecycleEvent>,
    shutdown: watch::Receiver<bool>,
    events: broadcast::Sender<LifecycleEvent>,
) {
    if !wait_until_ready(&mut ready_events, shutdown.clone()).await {
        return;
    }
    let mut attempt = 1_u32;
    loop {
        if *shutdown.borrow() {
            break;
        }
        emit(
            &events,
            LifecycleEvent::BackgroundWorkerStarting {
                name: spec.name.clone(),
                attempt,
            },
        );
        match spec.worker.run(WorkerContext::new(shutdown.clone())).await {
            Ok(()) => break,
            Err(error) if !*shutdown.borrow() => {
                emit(
                    &events,
                    LifecycleEvent::BackgroundWorkerFailed {
                        name: spec.name.clone(),
                        error,
                    },
                );
            }
            Err(_) => break,
        }
        let delay = spec.restart.delay(attempt);
        emit(
            &events,
            LifecycleEvent::BackgroundWorkerRestartScheduled {
                name: spec.name.clone(),
                attempt,
                delay,
            },
        );
        let mut restart_shutdown = shutdown.clone();
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            result = restart_shutdown.changed() => {
                if result.is_err() || *restart_shutdown.borrow() {
                    break;
                }
            }
        }
        attempt = attempt.saturating_add(1);
    }
    emit(
        &events,
        LifecycleEvent::BackgroundWorkerStopped { name: spec.name },
    );
}

async fn wait_until_ready(
    events: &mut broadcast::Receiver<LifecycleEvent>,
    mut shutdown: watch::Receiver<bool>,
) -> bool {
    loop {
        if *shutdown.borrow() {
            return false;
        }
        tokio::select! {
            event = events.recv() => match event {
                Ok(LifecycleEvent::Ready { .. }) => return true,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {},
                Err(broadcast::error::RecvError::Closed) => return false,
            },
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    return false;
                }
            }
        }
    }
}

fn emit(events: &broadcast::Sender<LifecycleEvent>, event: LifecycleEvent) {
    let _ = events.send(event);
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("shared database is not safe for Rust runtime use: {0}")]
    DatabaseAdmission(String),
    #[error("Discord gateway could not start: {0}")]
    Gateway(String),
}

#[cfg(test)]
mod tests {
    use std::future;

    use tempfile::NamedTempFile;

    use super::*;
    use crate::registration::RegistryBuilder;

    #[test]
    fn reconnect_backoff_doubles_and_caps() {
        let policy = ReconnectPolicy::new(Duration::from_secs(5), Duration::from_secs(300));
        assert_eq!(policy.delay(1), Duration::from_secs(5));
        assert_eq!(policy.delay(2), Duration::from_secs(10));
        assert_eq!(policy.delay(8), Duration::from_secs(300));
        assert_eq!(policy.delay(u32::MAX), Duration::from_secs(300));
    }

    #[test]
    fn intent_profile_preserves_python_privileged_intents() {
        let intents = GatewayIntentProfile::python_parity();
        assert!(intents.guild_members);
        assert!(intents.guild_presences);
        assert!(intents.message_content);
        assert!(intents.guild_message_reactions);
    }

    #[tokio::test]
    async fn sqlite_admission_serializes_concurrent_clean_database_migrations() {
        let database = NamedTempFile::new().expect("temporary database");
        let admission = SqliteDatabaseAdmission::default();
        let first = admission.admit(database.path());
        let second = admission.admit(database.path());
        let (first, second) = tokio::join!(first, second);
        let mut reports = [
            first.expect("first admission"),
            second.expect("second admission"),
        ];
        reports.sort_by_key(|report| report.newly_applied_migrations);

        assert_eq!(reports[0].newly_applied_migrations, 0);
        assert_eq!(
            reports[1].newly_applied_migrations,
            cama_db::expected_migrations().len()
        );
        assert!(reports.iter().all(|report| {
            report.applied_migrations == report.required_migrations
                && report.path == database.path()
        }));
    }

    #[tokio::test]
    async fn sqlite_admission_refuses_a_non_database_file() {
        let database = NamedTempFile::new().expect("temporary database");
        std::fs::write(database.path(), b"not a sqlite database").expect("malformed fixture");
        let error = SqliteDatabaseAdmission::default()
            .admit(database.path())
            .await
            .expect_err("malformed database must refuse startup");
        assert!(error.contains("SQLite") || error.contains("database"));
    }

    struct RejectingAdmission;

    #[async_trait]
    impl DatabaseAdmission for RejectingAdmission {
        async fn admit(&self, _path: &Path) -> Result<DatabaseAdmissionReport, String> {
            Err("injected admission refusal".to_owned())
        }
    }

    struct UnreachableGateway;

    #[async_trait]
    impl GatewayTransport for UnreachableGateway {
        async fn run_session(
            &mut self,
            _session: GatewaySession,
        ) -> Result<GatewaySessionEnd, GatewayError> {
            panic!("gateway must not start after database admission failure")
        }
    }

    #[tokio::test]
    async fn database_admission_failure_emits_stopped_for_health_reporter() {
        let config = RuntimeConfig {
            token: DiscordToken::parse("test-token").expect("test token"),
            db_path: "/tmp/rejected-cama.db".into(),
            reconnect_initial: Duration::ZERO,
            reconnect_max: Duration::ZERO,
            rust_cutover_candidate: false,
        };
        let runtime = Runtime::new(
            config,
            RegistryBuilder::default().build(),
            UnreachableGateway,
            RejectingAdmission,
        );
        let mut events = runtime.events().subscribe();
        assert!(matches!(
            runtime.run_until(future::pending()).await,
            Err(RuntimeError::DatabaseAdmission(message)) if message == "injected admission refusal"
        ));
        assert_eq!(
            events.recv().await.expect("starting event"),
            LifecycleEvent::Starting
        );
        assert_eq!(
            events.recv().await.expect("stopped event"),
            LifecycleEvent::Stopped
        );
    }
}
