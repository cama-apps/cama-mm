use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::watch;

use super::*;

/// Requests shutdown from inside the first sweep so the worker loop ends
/// without waiting on its wake interval.
struct StopAfterFirstSweep {
    sweeps: AtomicUsize,
    shutdown: watch::Sender<bool>,
}

#[async_trait]
impl ReadycheckSweepPort for StopAfterFirstSweep {
    async fn sweep_due_readychecks(&self) -> usize {
        self.sweeps.fetch_add(1, Ordering::SeqCst);
        self.shutdown.send(true).expect("worker holds the receiver");
        2
    }
}

#[tokio::test]
async fn worker_sweeps_on_wake_and_stops_on_shutdown() {
    let (sender, receiver) = watch::channel(false);
    let port = Arc::new(StopAfterFirstSweep {
        sweeps: AtomicUsize::new(0),
        shutdown: sender,
    });

    ReadycheckSweepWorker::new(port.clone())
        .run(WorkerContext::new(receiver))
        .await
        .expect("worker exits cleanly on shutdown");

    assert_eq!(port.sweeps.load(Ordering::SeqCst), 1);
    assert_eq!(READYCHECK_SWEEP_WORKER_NAME, "readycheck_sweep");
}
