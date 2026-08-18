use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cama_runtime::gateway::{DatabaseInitializationReport, GatewayError, GatewaySession};
use cama_runtime::{
    BackgroundWorker, BackgroundWorkerSpec, CommandSpec, DiscordToken, GatewaySessionEnd,
    GatewayTransport, GlobalInteractionHooks, InteractionHandler, InteractionHandlerError,
    InteractionRequest, InteractionResponder, LifecycleEvent, ReconnectPolicy, RegistryBuilder,
    Runtime, RuntimeConfig, UsageMonitor, WorkerContext,
};

fn initialized_database(path: &str) -> DatabaseInitializationReport {
    DatabaseInitializationReport {
        path: path.into(),
        applied_migrations: 42,
        required_migrations: 42,
        newly_applied_migrations: 0,
        created_tables: 0,
        rebuilt_tables: 0,
    }
}

struct RecordingHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl InteractionHandler for RecordingHandler {
    async fn handle(
        &self,
        _request: InteractionRequest,
        _responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct NoopResponder;

#[async_trait]
impl InteractionResponder for NoopResponder {
    async fn respond(
        &self,
        _response: cama_runtime::InteractionResponse,
    ) -> Result<(), cama_runtime::registration::InteractionResponseError> {
        Ok(())
    }

    async fn defer(
        &self,
        _ephemeral: bool,
    ) -> Result<(), cama_runtime::registration::InteractionResponseError> {
        Ok(())
    }

    async fn followup(
        &self,
        _response: cama_runtime::InteractionResponse,
    ) -> Result<(), cama_runtime::registration::InteractionResponseError> {
        Ok(())
    }
}

struct MockGateway {
    sessions: Arc<AtomicUsize>,
}

struct RestartingWorker {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl BackgroundWorker for RestartingWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            return Err("mock worker crash".to_owned());
        }
        context.cancelled().await;
        Ok(())
    }
}

#[async_trait]
impl GatewayTransport for MockGateway {
    async fn run_session(
        &mut self,
        mut session: GatewaySession,
    ) -> Result<GatewaySessionEnd, GatewayError> {
        let session_number = self.sessions.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(session.intents.guild_members);
        assert!(session.intents.guild_presences);
        assert!(session.intents.message_content);
        assert_eq!(session.registry.commands().len(), 1);
        assert!(session.global_interaction_hooks.is_some());

        if session_number == 1 {
            let handler = session
                .registry
                .command_handler("health")
                .expect("registered handler");
            handler
                .handle(
                    InteractionRequest::Command {
                        interaction_id: 1,
                        name: "health".to_owned(),
                        user_id: 2,
                        user_display_name: "health user".to_owned(),
                        guild_id: Some(3),
                        channel_id: Some(4),
                        member_permissions: None,
                        options: Vec::new(),
                    },
                    Arc::new(NoopResponder),
                )
                .await
                .expect("mock dispatch");
            let _ = session.events.send(LifecycleEvent::Ready {
                bot_user_id: 10,
                guild_count: 2,
            });
            let _ = session.events.send(LifecycleEvent::CommandsRegistered {
                command_count: 1,
                component_route_count: 0,
                synchronized: true,
            });
            return Ok(GatewaySessionEnd::Reconnect {
                reason: "mock websocket loss".to_owned(),
            });
        }

        loop {
            session
                .shutdown
                .changed()
                .await
                .map_err(|error| GatewayError::Fatal(error.to_string()))?;
            if *session.shutdown.borrow() {
                return Ok(GatewaySessionEnd::Shutdown);
            }
        }
    }
}

#[tokio::test]
async fn startup_ready_registration_reconnect_and_shutdown_are_supervised() {
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let mut registry = RegistryBuilder::default();
    registry
        .command(CommandSpec {
            name: "health".to_owned(),
            description: "Runtime health".to_owned(),
            options: Vec::new(),
            handler: Arc::new(RecordingHandler {
                calls: Arc::clone(&handler_calls),
            }),
        })
        .expect("register command");

    let sessions = Arc::new(AtomicUsize::new(0));
    let worker_calls = Arc::new(AtomicUsize::new(0));
    let config = RuntimeConfig {
        token: DiscordToken::parse("test-token").expect("test token"),
        db_path: "/tmp/cama-runtime-test.db".into(),
        reconnect_initial: Duration::ZERO,
        reconnect_max: Duration::ZERO,
    };
    let runtime = Runtime::new(
        config,
        registry.build(),
        MockGateway {
            sessions: Arc::clone(&sessions),
        },
        initialized_database("/tmp/cama-runtime-test.db"),
    )
    .with_global_interaction_hooks(GlobalInteractionHooks::new(UsageMonitor::default()))
    .with_worker(
        BackgroundWorkerSpec::new(
            "mock-worker",
            Arc::new(RestartingWorker {
                calls: Arc::clone(&worker_calls),
            }),
        )
        .restart_policy(ReconnectPolicy::new(Duration::ZERO, Duration::ZERO)),
    );
    let mut events = runtime.events().subscribe();
    let (stop_sender, stop_receiver) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(runtime.run_until(async move {
        let _ = stop_receiver.await;
    }));

    let mut observed = Vec::new();
    let mut second_connection = false;
    let mut worker_restarted = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("lifecycle event timeout")
            .expect("lifecycle channel");
        second_connection |= matches!(event, LifecycleEvent::Connecting { attempt: 2 });
        worker_restarted |= matches!(
            event,
            LifecycleEvent::BackgroundWorkerStarting {
                ref name,
                attempt: 2
            } if name == "mock-worker"
        );
        observed.push(event);
        if second_connection && worker_restarted {
            stop_sender.send(()).expect("request shutdown");
            break;
        }
    }

    task.await
        .expect("runtime task")
        .expect("clean runtime exit");
    while let Ok(event) = events.try_recv() {
        observed.push(event);
    }

    assert_eq!(sessions.load(Ordering::SeqCst), 2);
    assert_eq!(handler_calls.load(Ordering::SeqCst), 1);
    assert_eq!(worker_calls.load(Ordering::SeqCst), 2);
    assert!(matches!(observed[0], LifecycleEvent::Starting));
    assert!(
        observed
            .iter()
            .any(|event| matches!(event, LifecycleEvent::DatabaseReady { .. }))
    );
    assert!(
        observed
            .iter()
            .any(|event| matches!(event, LifecycleEvent::Ready { .. }))
    );
    assert!(observed.iter().any(|event| matches!(
        event,
        LifecycleEvent::CommandsRegistered {
            synchronized: true,
            ..
        }
    )));
    assert!(observed.iter().any(|event| matches!(event, LifecycleEvent::Disconnected { reason } if reason == "mock websocket loss")));
    assert!(
        observed
            .iter()
            .any(|event| matches!(event, LifecycleEvent::ReconnectScheduled { attempt: 1, .. }))
    );
    assert!(observed.iter().any(|event| matches!(event, LifecycleEvent::BackgroundWorkerFailed { name, error } if name == "mock-worker" && error == "mock worker crash")));
    assert!(observed.iter().any(|event| matches!(event, LifecycleEvent::BackgroundWorkerRestartScheduled { name, attempt: 1, .. } if name == "mock-worker")));
    assert!(observed.iter().any(|event| matches!(event, LifecycleEvent::BackgroundWorkerStopped { name } if name == "mock-worker")));
    assert!(
        observed
            .iter()
            .any(|event| matches!(event, LifecycleEvent::ShutdownRequested))
    );
    assert!(
        observed
            .iter()
            .any(|event| matches!(event, LifecycleEvent::Stopped))
    );
}

#[tokio::test]
async fn shutdown_during_worker_restart_backoff_prevents_stale_restart() {
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let mut registry = RegistryBuilder::default();
    registry
        .command(CommandSpec {
            name: "health".to_owned(),
            description: "Runtime health".to_owned(),
            options: Vec::new(),
            handler: Arc::new(RecordingHandler {
                calls: Arc::clone(&handler_calls),
            }),
        })
        .expect("register command");
    let sessions = Arc::new(AtomicUsize::new(0));
    let worker_calls = Arc::new(AtomicUsize::new(0));
    let config = RuntimeConfig {
        token: DiscordToken::parse("test-token").expect("test token"),
        db_path: "/tmp/cama-runtime-worker-shutdown.db".into(),
        reconnect_initial: Duration::ZERO,
        reconnect_max: Duration::ZERO,
    };
    let runtime = Runtime::new(
        config,
        registry.build(),
        MockGateway {
            sessions: Arc::clone(&sessions),
        },
        initialized_database("/tmp/cama-runtime-worker-shutdown.db"),
    )
    .with_global_interaction_hooks(GlobalInteractionHooks::new(UsageMonitor::default()))
    .with_worker(
        BackgroundWorkerSpec::new(
            "shutdown-worker",
            Arc::new(RestartingWorker {
                calls: Arc::clone(&worker_calls),
            }),
        )
        .restart_policy(ReconnectPolicy::new(
            Duration::from_secs(60),
            Duration::from_secs(60),
        )),
    );
    let mut events = runtime.events().subscribe();
    let (stop_sender, stop_receiver) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(runtime.run_until(async move {
        let _ = stop_receiver.await;
    }));

    let mut observed = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("lifecycle event timeout")
            .expect("lifecycle channel");
        let should_stop = matches!(
            event,
            LifecycleEvent::BackgroundWorkerRestartScheduled {
                ref name,
                attempt: 1,
                delay,
            } if name == "shutdown-worker" && delay == Duration::from_secs(60)
        );
        observed.push(event);
        if should_stop {
            stop_sender.send(()).expect("request shutdown");
            break;
        }
    }

    task.await
        .expect("runtime task")
        .expect("clean runtime exit");
    while let Ok(event) = events.try_recv() {
        observed.push(event);
    }

    assert!(sessions.load(Ordering::SeqCst) >= 1);
    assert_eq!(worker_calls.load(Ordering::SeqCst), 1);
    assert!(observed.iter().any(|event| matches!(
        event,
        LifecycleEvent::BackgroundWorkerStopped { name } if name == "shutdown-worker"
    )));
    assert!(!observed.iter().any(|event| matches!(
        event,
        LifecycleEvent::BackgroundWorkerStarting { name, attempt: 2 } if name == "shutdown-worker"
    )));
}
