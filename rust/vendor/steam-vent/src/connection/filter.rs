use crate::message::ServiceMethodNotification;
use crate::net::{JobId, RawNetMessage};
use dashmap::DashMap;
use futures_util::Stream;
use std::collections::VecDeque;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use steam_vent_proto_common::MsgKind;
use steam_vent_proto_steam::enums_clientserver::EMsg;
use tokio::spawn;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_stream::StreamExt;
use tracing::{debug, error};

#[derive(Clone)]
pub struct RingBuffer<T>(Arc<Mutex<VecDeque<T>>>);

impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(Mutex::new(VecDeque::with_capacity(capacity))))
    }

    pub fn push(&self, item: T) -> Option<T> {
        let mut deque = self.0.lock().unwrap();
        if deque.len() == deque.capacity() {
            let popped = deque.pop_front();
            deque.push_back(item);
            debug_assert!(deque.len() == deque.capacity());
            popped
        } else {
            deque.push_back(item);
            None
        }
    }

    #[allow(dead_code)]
    pub fn pop(&self) -> Option<T> {
        self.0.lock().unwrap().pop_front()
    }
}

impl<T: Clone> RingBuffer<T> {
    pub fn take(&self) -> Vec<T> {
        let mut dequeu = self.0.lock().unwrap();
        let items = dequeu.make_contiguous().to_vec();
        dequeu.clear();
        items
    }
}

/// A filter for incoming messages, allowing listing by message type, job id and notifications
#[derive(Clone)]
pub struct MessageFilter {
    job_id_filters: Arc<DashMap<JobId, oneshot::Sender<RawNetMessage>>>,
    job_id_multi_filters: Arc<DashMap<JobId, mpsc::Sender<RawNetMessage>>>,
    notification_filters: Arc<DashMap<&'static str, broadcast::Sender<ServiceMethodNotification>>>,
    kind_filters: Arc<DashMap<MsgKind, broadcast::Sender<RawNetMessage>>>,
    oneshot_kind_filters: Arc<DashMap<MsgKind, oneshot::Sender<RawNetMessage>>>,
    rest: RingBuffer<RawNetMessage>,
    // The worker owns the routing tables but not its own cancellation guard.
    worker: Option<Arc<WorkerGuard>>,
}

struct WorkerGuard(tokio::task::AbortHandle);

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) struct JobRegistration {
    filter: MessageFilter,
    id: JobId,
    multi: bool,
}

impl Drop for JobRegistration {
    fn drop(&mut self) {
        if self.multi {
            self.filter.job_id_multi_filters.remove(&self.id);
        } else {
            self.filter.job_id_filters.remove(&self.id);
        }
    }
}

impl MessageFilter {
    pub fn new<Input: Stream<Item = crate::connection::Result<RawNetMessage>> + Send + 'static>(
        source: Input,
    ) -> Self {
        let mut filter = MessageFilter {
            job_id_filters: Default::default(),
            job_id_multi_filters: Default::default(),
            kind_filters: Default::default(),
            notification_filters: Default::default(),
            oneshot_kind_filters: Default::default(),
            rest: RingBuffer::new(32),
            worker: None,
        };

        let filter_send = filter.clone();
        let worker = spawn(async move {
            let mut source = pin!(source);
            while let Some(res) = source.next().await {
                match res {
                    Ok(message) => {
                        debug!(job_id = message.header.target_job_id.0, kind = ?message.kind, "processing message");
                        if let Some((_, tx)) = filter_send
                            .job_id_filters
                            .remove(&message.header.target_job_id)
                        {
                            tx.send(message).ok();
                        } else if let Some(tx) = {
                            filter_send
                                .job_id_multi_filters
                                .get(&message.header.target_job_id)
                                .map(|entry| entry.value().clone())
                        } {
                            let id = message.header.target_job_id;
                            // A stalled subscriber must not block unrelated jobs.
                            // Close this response stream on overflow so it fails,
                            // rather than silently dropping a response fragment.
                            if tx.try_send(message).is_err() {
                                filter_send.job_id_multi_filters.remove(&id);
                            }
                        } else if let Some((_, tx)) =
                            filter_send.oneshot_kind_filters.remove(&message.kind)
                        {
                            tx.send(message).ok();
                        } else if message.kind == EMsg::k_EMsgServiceMethod {
                            if let Ok(notification) =
                                message.into_message::<ServiceMethodNotification>()
                            {
                                debug!(
                                    job_name = notification.job_name.as_str(),
                                    "processing notification"
                                );
                                if let Some(tx) = filter_send
                                    .notification_filters
                                    .get(notification.job_name.as_str())
                                {
                                    tx.send(notification).ok();
                                }
                            }
                        } else if let Some(tx) = filter_send.kind_filters.get(&message.kind) {
                            tx.send(message).ok();
                        } else if let Some(popped) = filter_send.rest.push(message) {
                            debug!(kind = ?popped.kind, "Unhandled message");
                        }
                    }
                    Err(err) => {
                        error!(error = ?err, "Error while reading message");
                    }
                }
            }
            filter_send.job_id_filters.clear();
            filter_send.job_id_multi_filters.clear();
            filter_send.oneshot_kind_filters.clear();
            filter_send.kind_filters.clear();
            filter_send.notification_filters.clear();
        });
        filter.worker = Some(Arc::new(WorkerGuard(worker.abort_handle())));
        filter
    }

    pub fn on_job_id(&self, id: JobId) -> oneshot::Receiver<RawNetMessage> {
        let (tx, rx) = oneshot::channel();
        self.job_id_filters.retain(|_, sender| !sender.is_closed());
        self.job_id_filters.insert(id, tx);
        rx
    }

    pub fn on_job_id_multi(&self, id: JobId) -> mpsc::Receiver<RawNetMessage> {
        let (tx, rx) = mpsc::channel(16);
        self.job_id_multi_filters
            .retain(|_, sender| !sender.is_closed());
        self.job_id_multi_filters.insert(id, tx);
        rx
    }

    pub(crate) fn job_registration(&self, id: JobId, multi: bool) -> JobRegistration {
        JobRegistration {
            filter: self.clone(),
            id,
            multi,
        }
    }

    pub fn complete_job_id_multi(&self, id: JobId) {
        self.job_id_multi_filters.remove(&id);
    }

    pub fn on_notification(
        &self,
        job_name: &'static str,
    ) -> broadcast::Receiver<ServiceMethodNotification> {
        let tx = self
            .notification_filters
            .entry(job_name)
            .or_insert_with(|| broadcast::channel(16).0);
        tx.subscribe()
    }

    pub fn on_kind<K: Into<MsgKind>>(&self, kind: K) -> broadcast::Receiver<RawNetMessage> {
        let tx = self
            .kind_filters
            .entry(kind.into())
            .or_insert_with(|| broadcast::channel(16).0);
        tx.subscribe()
    }

    pub fn one_kind<K: Into<MsgKind>>(&self, kind: K) -> oneshot::Receiver<RawNetMessage> {
        let (tx, rx) = oneshot::channel();
        self.oneshot_kind_filters.insert(kind.into(), tx);
        rx
    }

    pub fn unprocessed(&self) -> Vec<RawNetMessage> {
        self.rest.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn message(id: JobId) -> RawNetMessage {
        let mut message = RawNetMessage::read(vec![1, 0, 0, 128, 0, 0, 0, 0]).unwrap();
        message.header.target_job_id = id;
        message
    }

    #[tokio::test]
    async fn dropped_registration_removes_pending_job_immediately() {
        let filter = MessageFilter::new(stream::pending());
        let _recv = filter.on_job_id(JobId(1));
        let registration = filter.job_registration(JobId(1), false);
        assert_eq!(filter.job_id_filters.len(), 1);
        drop(registration);
        assert!(filter.job_id_filters.is_empty());
        let _multi = filter.on_job_id_multi(JobId(2));
        drop(filter.job_registration(JobId(2), true));
        assert!(filter.job_id_multi_filters.is_empty());
    }

    #[tokio::test]
    async fn closed_receivers_are_pruned_when_registering_another_job() {
        let filter = MessageFilter::new(stream::pending());
        for id in 0..100 {
            drop(filter.on_job_id(JobId(id)));
            drop(filter.on_job_id_multi(JobId(id)));
        }
        assert_eq!(filter.job_id_filters.len(), 1);
        assert_eq!(filter.job_id_multi_filters.len(), 1);
    }

    #[tokio::test]
    async fn full_multi_queue_does_not_stall_other_jobs() {
        let (tx, rx) = mpsc::channel(32);
        let filter = MessageFilter::new(tokio_stream::wrappers::ReceiverStream::new(rx));
        let mut multi = filter.on_job_id_multi(JobId(1));
        let other = filter.on_job_id(JobId(2));
        for _ in 0..17 {
            tx.send(Ok(message(JobId(1)))).await.unwrap();
        }
        tx.send(Ok(message(JobId(2)))).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), other)
                .await
                .unwrap()
                .is_ok()
        );
        for _ in 0..16 {
            assert!(multi.recv().await.is_some());
        }
        assert!(multi.recv().await.is_none());
    }

    #[tokio::test]
    async fn ending_source_closes_pending_requests() {
        let (tx, rx) = mpsc::channel(1);
        let filter = MessageFilter::new(tokio_stream::wrappers::ReceiverStream::new(rx));
        let recv = filter.on_job_id(JobId(1));
        drop(tx);
        assert!(recv.await.is_err());
    }

    struct DropMarker(Arc<AtomicBool>);
    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn dropping_last_handle_cancels_worker_and_releases_source() {
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = DropMarker(dropped.clone());
        let source = stream::unfold(marker, |marker| async move {
            futures_util::future::pending::<()>().await;
            Some((Ok(message(JobId(1))), marker))
        });
        let filter = MessageFilter::new(source);
        tokio::task::yield_now().await;
        let clone = filter.clone();
        drop(filter);
        assert!(!dropped.load(Ordering::SeqCst));
        drop(clone);
        tokio::task::yield_now().await;
        assert!(dropped.load(Ordering::SeqCst));
    }
}
