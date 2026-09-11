use super::Result;
use crate::connection::{ConnectionImpl, MessageFilter, MessageSender};
use crate::message::{EncodableMessage, flatten_multi};
use crate::net::{NetMessageHeader, RawNetMessage};
use crate::session::{Session, hello};
use crate::transport::websocket::connect;
use crate::{ConnectionError, NetworkError, ServerList};
use bytes::BytesMut;
use futures_util::{Sink, SinkExt, Stream};
use std::fmt::{Debug, Formatter};
use std::future::ready;
use std::sync::Arc;
use std::time::Duration;
use steam_vent_proto_common::MsgKindEnum;
use steam_vent_proto_steam::steammessages_clientserver_login::CMsgClientHeartBeat;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio::{select, spawn};
use tokio_stream::StreamExt;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{debug, error};

#[derive(Clone)]
pub(crate) struct RawConnection {
    pub session: Session,
    pub filter: MessageFilter,
    pub timeout: Duration,
    pub sender: MessageSender,
    heartbeat_cancellation_token: CancellationToken,
    _heartbeat_drop_guard: Arc<DropGuard>,
}

impl Debug for RawConnection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawConnection").finish_non_exhaustive()
    }
}

impl RawConnection {
    pub async fn connect(server_list: &ServerList) -> Result<Self, ConnectionError> {
        let (sender, receiver) = connect(&server_list.pick_ws()).await?;
        Self::from_sender_receiver(sender, receiver).await
    }

    pub async fn from_sender_receiver<
        Sender: Sink<BytesMut, Error = NetworkError> + Send + 'static,
        Receiver: Stream<Item = Result<BytesMut>> + Send + 'static,
    >(
        sender: Sender,
        receiver: Receiver,
    ) -> Result<Self, ConnectionError> {
        let sender = sender.with(|msg: RawNetMessage| ready(Ok(msg.into_bytes())));
        let receiver = flatten_multi(receiver.map(|res| res.and_then(RawNetMessage::read)));

        let filter = MessageFilter::new(receiver);
        let heartbeat_cancellation_token = CancellationToken::new();
        let mut connection = RawConnection {
            session: Session::default(),
            filter,
            sender: MessageSender {
                write: Arc::new(Mutex::new(Box::pin(sender))),
            },
            timeout: Duration::from_secs(10),
            heartbeat_cancellation_token: heartbeat_cancellation_token.clone(),
            // We just store a drop guard using an `Arc` here, so dropping the last clone of `Connection` will cancel the heartbeat task.
            _heartbeat_drop_guard: Arc::new(heartbeat_cancellation_token.drop_guard()),
        };
        hello(&mut connection).await?;
        Ok(connection)
    }

    pub fn setup_heartbeat(&self) {
        let sender = self.sender.clone();
        let interval = self.session.heartbeat_interval;
        let header = NetMessageHeader {
            session_id: self.session.session_id,
            steam_id: self.session().steam_id,
            ..NetMessageHeader::default()
        };
        debug!("Setting up heartbeat with interval {:?}", interval);
        let token = self.heartbeat_cancellation_token.clone();
        spawn(async move {
            loop {
                select! {
                    _ = sleep(interval) => {},
                    _ = token.cancelled() => {
                        break
                    }
                };
                debug!("Sending heartbeat message");
                match RawNetMessage::from_message(header.clone(), CMsgClientHeartBeat::default()) {
                    Ok(msg) => {
                        let result = select! {
                            _ = token.cancelled() => break,
                            result = sender.send_raw(msg) => result,
                        };
                        if let Err(e) = result {
                            error!(error = ?e, "Failed to send heartbeat message");
                            break;
                        }
                    }
                    Err(e) => {
                        error!(error = ?e, "Failed to prepare heartbeat message")
                    }
                }
            }
            debug!("Heartbeat task stopping");
        });
    }
}

impl ConnectionImpl for RawConnection {
    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn filter(&self) -> &MessageFilter {
        &self.filter
    }

    fn session(&self) -> &Session {
        &self.session
    }

    async fn raw_send_with_kind<Msg: EncodableMessage, K: MsgKindEnum>(
        &self,
        header: NetMessageHeader,
        msg: Msg,
        kind: K,
        is_protobuf: bool,
    ) -> Result<()> {
        let msg = RawNetMessage::from_message_with_kind(header, msg, kind, is_protobuf)?;
        self.sender.send_raw(msg).await
    }
}

#[cfg(test)]
mod heartbeat_cancellation_tests {
    use super::*;
    use futures_util::{sink, stream};
    use tokio::sync::oneshot;

    struct DropSignal(Option<oneshot::Sender<()>>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[tokio::test]
    async fn disconnect_cancels_heartbeat_even_when_transport_write_is_stalled() {
        let (entered, entered_rx) = oneshot::channel();
        let (dropped, dropped_rx) = oneshot::channel();
        let sink = sink::unfold(
            (Some(entered), DropSignal(Some(dropped))),
            |(mut entered, marker), _message: RawNetMessage| async move {
                let _ = entered.take().unwrap().send(());
                futures_util::future::pending::<()>().await;
                Ok::<_, NetworkError>((entered, marker))
            },
        );
        let token = CancellationToken::new();
        let connection = RawConnection {
            session: Session {
                heartbeat_interval: Duration::from_millis(1),
                ..Session::default()
            },
            filter: MessageFilter::new(stream::pending()),
            timeout: Duration::from_secs(1),
            sender: MessageSender {
                write: Arc::new(Mutex::new(Box::pin(sink))),
            },
            heartbeat_cancellation_token: token.clone(),
            _heartbeat_drop_guard: Arc::new(token.drop_guard()),
        };
        connection.setup_heartbeat();
        tokio::time::timeout(Duration::from_secs(1), entered_rx)
            .await
            .unwrap()
            .unwrap();
        drop(connection);
        // The stalled future owns DropSignal. Its destruction proves the worker
        // actually stopped and released the writer, instead of merely flagging it.
        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .unwrap()
            .unwrap();
    }
}
