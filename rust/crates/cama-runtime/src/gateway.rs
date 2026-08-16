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
pub struct DatabaseInitializationReport {
    pub path: PathBuf,
    pub applied_migrations: usize,
    pub required_migrations: usize,
    pub newly_applied_migrations: usize,
    pub created_tables: usize,
    pub rebuilt_tables: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SqliteDatabaseInitializer {
    migration_settings: MigrationSettings,
}

impl SqliteDatabaseInitializer {
    #[must_use]
    pub const fn with_migration_settings(migration_settings: MigrationSettings) -> Self {
        Self { migration_settings }
    }

    #[must_use]
    pub const fn migration_settings(&self) -> &MigrationSettings {
        &self.migration_settings
    }

    /// Bring the SQLite schema to the version embedded in this binary and
    /// verify the canonical runtime storage contract before services are built.
    pub async fn initialize(&self, path: &Path) -> Result<DatabaseInitializationReport, String> {
        let path = path.to_path_buf();
        let settings = self.migration_settings.clone();
        tokio::task::spawn_blocking(move || -> Result<DatabaseInitializationReport, String> {
            let migration = initialize_or_migrate_with_settings(&path, &settings)
                .map_err(|error| error.to_string())?;
            let audit = audit_database(&path).map_err(|error| error.to_string())?;
            if !audit.is_compatible() {
                return Err(audit.issues().join("; "));
            }
            Ok(DatabaseInitializationReport {
                path: audit.path,
                applied_migrations: audit.applied_migration_count,
                required_migrations: audit.required_migration_count,
                newly_applied_migrations: migration.newly_applied.len(),
                created_tables: migration.created_tables.len(),
                rebuilt_tables: migration.rebuilt_tables.len(),
            })
        })
        .await
        .map_err(|error| format!("database initialization task failed: {error}"))?
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

pub struct Runtime<G> {
    config: RuntimeConfig,
    registry: Arc<Registry>,
    gateway: G,
    database: DatabaseInitializationReport,
    reconnect: ReconnectPolicy,
    events: broadcast::Sender<LifecycleEvent>,
    workers: Vec<BackgroundWorkerSpec>,
    observers: GatewayEventObservers,
    global_interaction_hooks: Option<GlobalInteractionHooks>,
    raw_reaction_observers: RawReactionObservers,
}

impl<G> Runtime<G>
where
    G: GatewayTransport,
{
    #[must_use]
    pub fn new(
        config: RuntimeConfig,
        registry: Registry,
        gateway: G,
        database: DatabaseInitializationReport,
    ) -> Self {
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

    /// Attach production gateway observers after the initialized service graph
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
        if self.database.path != self.config.db_path {
            emit(&self.events, LifecycleEvent::Stopped);
            return Err(RuntimeError::DatabaseInitialization(format!(
                "completed schema initialization is for {}, not {}",
                self.database.path.display(),
                self.config.db_path.display()
            )));
        }
        emit(
            &self.events,
            LifecycleEvent::DatabaseReady {
                path: self.database.path.clone(),
                applied_migrations: self.database.applied_migrations,
                required_migrations: self.database.required_migrations,
                newly_applied_migrations: self.database.newly_applied_migrations,
                created_tables: self.database.created_tables,
                rebuilt_tables: self.database.rebuilt_tables,
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

            // A session that reached Ready resets the exponential backoff,
            // matching discord.py's reconnect behavior. The observer is
            // polled concurrently with the running session so a long
            // session's lifecycle traffic can never evict the single Ready
            // event from the bounded broadcast channel before it is seen.
            let mut ready_observer = self.events.subscribe();
            let mut reached_ready = false;
            let session_end = {
                let mut session_future = std::pin::pin!(self.gateway.run_session(session));
                loop {
                    tokio::select! {
                        result = &mut session_future => break result,
                        event = ready_observer.recv() => {
                            if matches!(event, Ok(LifecycleEvent::Ready { .. })) {
                                reached_ready = true;
                            }
                        }
                    }
                }
            };
            // Catch events emitted between the observer's last poll and the
            // session future completing.
            loop {
                match ready_observer.try_recv() {
                    Ok(LifecycleEvent::Ready { .. }) => reached_ready = true,
                    Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(
                        broadcast::error::TryRecvError::Empty
                        | broadcast::error::TryRecvError::Closed,
                    ) => break,
                }
            }

            let reconnect_reason = match session_end {
                Ok(GatewaySessionEnd::Shutdown) => break,
                Ok(GatewaySessionEnd::Reconnect { reason }) => reason,
                Err(GatewayError::Recoverable(reason)) => reason,
                Err(GatewayError::Fatal(reason)) => {
                    terminal_error = Some(RuntimeError::Gateway(reason));
                    break;
                }
            };
            if reached_ready {
                // Restart the backoff sequence exactly as at process start:
                // the delay below becomes the initial delay again.
                attempt = 1;
            }

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

/// A worker run that survived this long before failing is treated as having
/// been healthy, so its restart backoff starts over instead of compounding
/// across the process lifetime.
const WORKER_BACKOFF_RESET_UPTIME: Duration = Duration::from_secs(60);

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
        let started = tokio::time::Instant::now();
        match spec.worker.run(WorkerContext::new(shutdown.clone())).await {
            Ok(()) => break,
            Err(error) if !*shutdown.borrow() => {
                if started.elapsed() >= WORKER_BACKOFF_RESET_UPTIME {
                    attempt = 1;
                }
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
    #[error("database schema initialization is not valid for this runtime: {0}")]
    DatabaseInitialization(String),
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
    async fn sqlite_initialization_serializes_concurrent_clean_database_migrations() {
        let database = NamedTempFile::new().expect("temporary database");
        let initializer = SqliteDatabaseInitializer::default();
        let first = initializer.initialize(database.path());
        let second = initializer.initialize(database.path());
        let (first, second) = tokio::join!(first, second);
        let mut reports = [
            first.expect("first initialization"),
            second.expect("second initialization"),
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
    async fn sqlite_initialization_refuses_a_non_database_file() {
        let database = NamedTempFile::new().expect("temporary database");
        std::fs::write(database.path(), b"not a sqlite database").expect("malformed fixture");
        let error = SqliteDatabaseInitializer::default()
            .initialize(database.path())
            .await
            .expect_err("malformed database must refuse startup");
        assert!(error.contains("SQLite") || error.contains("database"));
    }

    fn initialized_database(path: impl Into<PathBuf>) -> DatabaseInitializationReport {
        DatabaseInitializationReport {
            path: path.into(),
            applied_migrations: 1,
            required_migrations: 1,
            newly_applied_migrations: 0,
            created_tables: 0,
            rebuilt_tables: 0,
        }
    }

    struct ScriptedGateway {
        calls: u32,
    }

    #[async_trait]
    impl GatewayTransport for ScriptedGateway {
        async fn run_session(
            &mut self,
            session: GatewaySession,
        ) -> Result<GatewaySessionEnd, GatewayError> {
            self.calls += 1;
            match self.calls {
                // Two consecutive connect failures escalate the backoff.
                1 | 2 => Err(GatewayError::Recoverable("connect refused".to_owned())),
                // A session that reached Ready must reset the backoff, even
                // when later lifecycle traffic overflows the bounded
                // broadcast channel (Ready must not be evicted unseen).
                3 => {
                    emit(
                        &session.events,
                        LifecycleEvent::Ready {
                            bot_user_id: 7,
                            guild_count: 1,
                        },
                    );
                    for _ in 0..300 {
                        emit(&session.events, LifecycleEvent::Resumed);
                        tokio::task::yield_now().await;
                    }
                    Ok(GatewaySessionEnd::Reconnect {
                        reason: "healthy session dropped".to_owned(),
                    })
                }
                _ => Ok(GatewaySessionEnd::Shutdown),
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn reconnect_backoff_resets_after_a_ready_session() {
        let config = RuntimeConfig {
            token: DiscordToken::parse("test-token").expect("test token"),
            db_path: "/tmp/backoff-reset-cama.db".into(),
            reconnect_initial: Duration::from_secs(5),
            reconnect_max: Duration::from_secs(300),
            rust_cutover_candidate: false,
        };
        let runtime = Runtime::new(
            config,
            RegistryBuilder::default().build(),
            ScriptedGateway { calls: 0 },
            initialized_database("/tmp/backoff-reset-cama.db"),
        );
        // Collect concurrently: the scripted session floods the bounded
        // broadcast channel, so a drain-at-the-end receiver would itself
        // lose the ReconnectScheduled events under test.
        let mut events = runtime.events().subscribe();
        let scheduled = Arc::new(std::sync::Mutex::new(Vec::new()));
        let collector = tokio::spawn({
            let scheduled = Arc::clone(&scheduled);
            async move {
                loop {
                    match events.recv().await {
                        Ok(LifecycleEvent::ReconnectScheduled { attempt, delay }) => scheduled
                            .lock()
                            .expect("collector mutex")
                            .push((attempt, delay)),
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        });
        runtime
            .run_until(future::pending())
            .await
            .expect("scripted runtime completes");
        collector.await.expect("collector completes");

        let scheduled = Arc::try_unwrap(scheduled)
            .expect("collector released its handle")
            .into_inner()
            .expect("collector mutex");
        assert_eq!(
            scheduled,
            [
                (1, Duration::from_secs(5)),
                (2, Duration::from_secs(10)),
                // The Ready session restarted the sequence at the initial
                // delay instead of continuing to 20 seconds.
                (1, Duration::from_secs(5)),
            ]
        );
    }

    struct UnreachableGateway;

    #[async_trait]
    impl GatewayTransport for UnreachableGateway {
        async fn run_session(
            &mut self,
            _session: GatewaySession,
        ) -> Result<GatewaySessionEnd, GatewayError> {
            panic!("gateway must not start after database initialization mismatch")
        }
    }

    #[tokio::test]
    async fn mismatched_database_initialization_emits_stopped_for_health_reporter() {
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
            initialized_database("/tmp/different-cama.db"),
        );
        let mut events = runtime.events().subscribe();
        assert!(matches!(
            runtime.run_until(future::pending()).await,
            Err(RuntimeError::DatabaseInitialization(message))
                if message.contains("/tmp/different-cama.db")
                    && message.contains("/tmp/rejected-cama.db")
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
